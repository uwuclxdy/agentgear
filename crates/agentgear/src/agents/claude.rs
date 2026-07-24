//! The Claude Code backend: converge the `claude plugin` registry to a desired
//! state via marketplace-ensure + install/update, read back through `list --json`.
//! It orchestrates the supported CLI; it never forges CC's on-disk state.
//!
//! One exception to "no config file": CC's `statusLine` slot lives in the user's
//! own `settings.json`, not in a plugin tree, so a host that declares a status line
//! gets it written here through the shared [`confedit`] read-modify-write. The slot
//! holds a single value and is last-writer-wins, so reconcile stashes whatever was
//! there into the stamp marker and remove puts it back — but only while the live
//! value is still one of ours, which [`is_ours`] decides on the command string.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{AgentBackend, BackendState, confedit};
use crate::cli::{ClaudeCli, version_lt};
use crate::components::expand_client;
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};
use crate::manifest::{MarketplaceEntry, PluginEntry};
use crate::materialize::{TreeSource, materialize};
use crate::stamp;
use crate::statusline::StatusLineDecl;

/// CC's settings key for the status-line slot. A single object
/// (`{"type":"command","command":…}`), never an array.
const STATUSLINE_KEY: &str = "statusLine";

pub(crate) struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn detect(&self) -> bool {
        which::which("claude").is_ok()
    }

    fn capabilities(&self) -> Capabilities {
        // Plugin-native: the `claude plugin` install copies the whole CC tree, so
        // every surface is served natively. `instructions` stays false: it is the
        // non-CC context-file surface, and CC receives host guidance via the MCP
        // `instructions` channel instead. `statusline` is true and NOT implied by
        // `plugins`: the slot lives in the user's settings.json, outside any tree.
        Capabilities {
            plugins: true,
            mcp: true,
            hooks: true,
            commands: true,
            agents: true,
            skills: true,
            instructions: false,
            statusline: true,
            scopes: &["user", "project"],
        }
    }

    /// The classification self_heal keys on: disabled beats broken beats stale.
    /// Mirrors selfheal.rs's inline logic (moved here so pass B can delegate to it).
    fn probe(&self, plugin: &Plugin, scope: &Scope, _source: &Source) -> Result<BackendState> {
        // CLI-based: the `claude plugin` registry is the source of truth, so the
        // resolved `source` (materialize's input) never enters this probe.
        let cli = ClaudeCli::locate()?;
        let Some(entry) = find_plugin(&cli, scope, plugin.name, plugin.marketplace)? else {
            return Ok(BackendState::Absent);
        };
        if entry.enabled == Some(false) {
            return Ok(BackendState::Disabled);
        }
        let files_ok = entry.install_path.as_ref().is_none_or(|p| Path::new(p).exists());
        let monotonic_current = !version_lt(entry.version.as_deref(), plugin.version);
        let registry = if files_ok && monotonic_current { BackendState::Healthy } else { BackendState::NeedsRepair };
        // The registry alone decides presence. A statusLine of ours still sitting in
        // settings.json after a manual `claude plugin uninstall` must not read as
        // "present but drifted", or self_heal would resurrect a deliberate uninstall
        // (the Absent arm above already returned). Once the plugin IS registered, a
        // missing or foreign statusLine is drift like any other surface.
        Ok(match statusline_state(plugin, scope)? {
            None | Some(BackendState::Healthy) => registry,
            Some(_) => BackendState::NeedsRepair,
        })
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        reconcile(plugin, desired, scope)
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, _source: &Source) -> Result<Outcome> {
        remove(plugin, scope)
    }

    /// CC's statusLine slot is our only write outside the plugin registry, so a
    /// plugin the user removed by hand leaves our command behind with nothing left
    /// to restore it once the marker (and its stash) goes.
    fn forget(&self, plugin: &Plugin, scope: &Scope) -> Result<()> {
        statusline_remove(plugin, scope).map(|_| ())
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        // The claude-specific checks only; the doctor fan-out owns the shared
        // host-binary check (calling `doctor` here would recurse through `report`).
        crate::doctor::claude_report(plugin, source)
    }
}

// --- state reads -------------------------------------------------------------

pub(crate) fn find_plugin(cli: &ClaudeCli, scope: &Scope, name: &str, marketplace: &str) -> Result<Option<PluginEntry>> {
    let entries: Vec<PluginEntry> = cli.run_json(&["plugin", "list", "--json"], scope.cwd(), "plugin list --json")?;
    Ok(entries.into_iter().find(|e| e.matches(name, marketplace)))
}

pub(crate) fn find_marketplace(cli: &ClaudeCli, scope: &Scope, marketplace: &str) -> Result<Option<MarketplaceEntry>> {
    let entries: Vec<MarketplaceEntry> =
        cli.run_json(&["plugin", "marketplace", "list", "--json"], scope.cwd(), "marketplace list --json")?;
    Ok(entries.into_iter().find(|m| m.name.as_deref() == Some(marketplace)))
}

fn marketplace_has_installed(cli: &ClaudeCli, scope: &Scope, marketplace: &str) -> Result<bool> {
    let entries: Vec<PluginEntry> = cli.run_json(&["plugin", "list", "--json"], scope.cwd(), "plugin list --json")?;
    Ok(entries.iter().any(|e| e.marketplace() == Some(marketplace)))
}

// --- mutating calls ----------------------------------------------------------

fn marketplace_add(cli: &ClaudeCli, source: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "marketplace", "add", source, "--scope", scope.as_cli()], scope.cwd())?;
    Ok(())
}

fn marketplace_update(cli: &ClaudeCli, marketplace: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "marketplace", "update", marketplace], scope.cwd())?;
    Ok(())
}

fn marketplace_remove(cli: &ClaudeCli, marketplace: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "marketplace", "remove", marketplace, "--scope", scope.as_cli()], scope.cwd())?;
    Ok(())
}

fn plugin_install(cli: &ClaudeCli, id: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "install", id, "--scope", scope.as_cli()], scope.cwd())?;
    Ok(())
}

fn plugin_update(cli: &ClaudeCli, id: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "update", id, "--scope", scope.as_cli()], scope.cwd())?;
    Ok(())
}

fn plugin_uninstall(cli: &ClaudeCli, id: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "uninstall", id, "-y", "--scope", scope.as_cli()], scope.cwd())?;
    Ok(())
}

fn plugin_enable(cli: &ClaudeCli, id: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "enable", id, "--scope", scope.as_cli()], scope.cwd())?;
    Ok(())
}

// --- reconcile ---------------------------------------------------------------

pub(crate) fn reconcile(plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
    match reconcile_registry(plugin, desired, scope)? {
        // Frozen: someone else owns this install, so its statusLine is theirs too.
        RegistryOutcome::Frozen => Ok(Outcome::NoOp),
        RegistryOutcome::Converged(outcome) => {
            let changed = statusline_reconcile(plugin, desired, scope)?;
            // A drifted statusLine behind an otherwise-converged registry is still a
            // repair; any real registry change already outranks it.
            Ok(match (outcome, changed) {
                (Outcome::NoOp, true) => Outcome::Repaired,
                (outcome, _) => outcome,
            })
        }
    }
}

/// What the plugin-registry half of a reconcile settled on. `Frozen` marks a state
/// this binary must not touch at all — a strictly-newer install (a newer binary owns
/// it) or a deliberate disable on a non-reenabling pass — so the statusLine half is
/// skipped with it rather than writing over another owner's slot.
enum RegistryOutcome {
    Converged(Outcome),
    Frozen,
}

fn reconcile_registry(plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<RegistryOutcome> {
    let cli = ClaudeCli::locate()?;
    let marketplace = find_marketplace(&cli, scope, plugin.marketplace)?;
    let entry = find_plugin(&cli, scope, plugin.name, plugin.marketplace)?;
    let id = plugin.id();

    let Some(entry) = entry else {
        // Absent: full install.
        cli.ensure_min_version()?;
        ensure_marketplace(&cli, plugin, &desired.source, scope, marketplace.as_ref())?;
        plugin_install(&cli, &id, scope)?;
        verify_present(&cli, scope, plugin)?;
        return Ok(RegistryOutcome::Converged(Outcome::Installed));
    };

    if entry.enabled == Some(false) {
        // An explicit install/update honors the user's intent and re-enables (the
        // design says install flips enable state). self_heal/adopt never does.
        if !desired.reenable {
            return Ok(RegistryOutcome::Frozen);
        }
        cli.ensure_min_version()?;
        plugin_enable(&cli, &id, scope)?;
        return Ok(RegistryOutcome::Converged(Outcome::Repaired));
    }

    let installed = entry.version.clone();
    let stale = version_lt(installed.as_deref(), plugin.version);
    let newer = installed.as_deref().is_some_and(|v| version_lt(Some(plugin.version), v));
    let structural_ok = structural_ok(&entry, marketplace.as_ref(), &desired.source);

    // Monotonic: a strictly-newer install belongs to a newer binary. Never touch
    // it — not even to repair a broken one — or two coexisting binaries downgrade
    // each other on every session. The newer binary owns its own repair.
    if newer {
        return Ok(RegistryOutcome::Frozen);
    }

    if structural_ok && !stale {
        // Healthy and monotonic-satisfied (installed == embedded).
        return Ok(RegistryOutcome::Converged(Outcome::NoOp));
    }

    cli.ensure_min_version()?;
    ensure_marketplace(&cli, plugin, &desired.source, scope, marketplace.as_ref())?;

    if stale && structural_ok {
        plugin_update(&cli, &id, scope)?;
        verify_present(&cli, scope, plugin)?;
        Ok(RegistryOutcome::Converged(Outcome::Updated { from: installed, to: plugin.version.to_string() }))
    } else {
        // Structurally broken (registered but files/marketplace gone): clean reinstall.
        let _ = plugin_uninstall(&cli, &id, scope);
        plugin_install(&cli, &id, scope)?;
        verify_present(&cli, scope, plugin)?;
        Ok(RegistryOutcome::Converged(Outcome::Repaired))
    }
}

/// Embedded/path: (re)materialize so `current@claude` is fresh, then add-if-absent /
/// update-if-present. GitHub: send `owner/repo@ref` to pin the ref; when already
/// present, `update` if the stored ref still matches, else re-`add` to re-point the
/// pin (`update` never moves one — design §ref-pinning ground truth). Ceiling: an
/// install predating client-scoping stays registered on a plain `current`; `update`
/// refreshes that stale path, so it must be reinstalled to pick up client-scoped
/// staging.
fn ensure_marketplace(cli: &ClaudeCli, plugin: &Plugin, source: &Source, scope: &Scope, present: Option<&MarketplaceEntry>) -> Result<()> {
    // Client-scope the materialization under this backend's own id, so CC and copilot
    // never collide on the shared data root (each bakes its own `${AGENTGEAR_CLIENT}`).
    let client = ClaudeBackend.id();
    let source_str = match source {
        Source::Embedded => materialize(plugin, TreeSource::Blob(plugin.blob()), client)?.display().to_string(),
        // A path source materializes its on-disk tree the same way embedded does.
        Source::Path(p) => materialize(plugin, TreeSource::Dir(p), client)?.display().to_string(),
        Source::GitHub { repo, ref_ } => github_source(repo, ref_),
    };
    match marketplace_op(source, present) {
        MarketplaceOp::Update => marketplace_update(cli, plugin.marketplace, scope)?,
        MarketplaceOp::Add => marketplace_add(cli, &source_str, scope)?,
    }
    Ok(())
}

/// The `marketplace add` source string a github pin sends: `owner/repo@ref`, which
/// the CLI resolves by checking `ref` out (design §ref-pinning ground truth). A bare
/// `owner/repo` silently tracks the default branch instead.
fn github_source(repo: &str, ref_: &str) -> String {
    format!("{repo}@{ref_}")
}

/// Present-branch decision. `Add` re-registers the marketplace: install-if-absent,
/// or re-point a github pin whose stored ref drifted from the desired one (`update`
/// never moves a pin). `Update` refreshes an already-present marketplace sitting on
/// its desired ref, and every non-github source.
#[derive(Debug, PartialEq, Eq)]
enum MarketplaceOp {
    Add,
    Update,
}

fn marketplace_op(source: &Source, present: Option<&MarketplaceEntry>) -> MarketplaceOp {
    match (source, present) {
        (_, None) => MarketplaceOp::Add,
        (Source::GitHub { ref_, .. }, Some(entry)) if entry.ref_.as_deref() != Some(*ref_) => MarketplaceOp::Add,
        (_, Some(_)) => MarketplaceOp::Update,
    }
}

/// GitHub entries have no local path to dangle (the registry stores `source:
/// github` + a ref), so a present github entry is always healthy. A missing local
/// `path` field reads as healthy, matching the tolerant serde model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarketplaceHealth {
    Healthy,
    Absent,
    Dangling,
}

pub(crate) fn marketplace_health(marketplace: Option<&MarketplaceEntry>, source: &Source) -> MarketplaceHealth {
    let Some(m) = marketplace else {
        return MarketplaceHealth::Absent;
    };
    match source {
        Source::Embedded | Source::Path(_) => {
            if m.path.as_ref().is_none_or(|p| Path::new(p).exists()) {
                MarketplaceHealth::Healthy
            } else {
                MarketplaceHealth::Dangling
            }
        }
        Source::GitHub { .. } => MarketplaceHealth::Healthy,
    }
}

fn structural_ok(entry: &PluginEntry, marketplace: Option<&MarketplaceEntry>, source: &Source) -> bool {
    let files_ok = entry.install_path.as_ref().is_none_or(|p| Path::new(p).exists());
    let marketplace_ok = matches!(marketplace_health(marketplace, source), MarketplaceHealth::Healthy);
    files_ok && marketplace_ok
}

fn verify_present(cli: &ClaudeCli, scope: &Scope, plugin: &Plugin) -> Result<()> {
    match find_plugin(cli, scope, plugin.name, plugin.marketplace)? {
        Some(e) if e.install_path.as_ref().is_none_or(|p| Path::new(p).exists()) => Ok(()),
        Some(_) => Err(Error::Verify(format!("{} registered but its files are missing after the operation", plugin.id()))),
        None => Err(Error::Verify(format!("{} absent from `plugin list --json` after the operation", plugin.id()))),
    }
}

// --- remove ------------------------------------------------------------------

pub(crate) fn remove(plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
    let cli = ClaudeCli::locate()?;
    let id = plugin.id();

    if find_plugin(&cli, scope, plugin.name, plugin.marketplace)?.is_some() {
        plugin_uninstall(&cli, &id, scope)?;
    }

    // Refcount-gated: only drop the marketplace when no plugin from it remains.
    if !marketplace_has_installed(&cli, scope, plugin.marketplace)? && find_marketplace(&cli, scope, plugin.marketplace)?.is_some() {
        let _ = marketplace_remove(&cli, plugin.marketplace, scope);
    }
    // The statusLine slot lives in the user's settings.json, outside the plugin
    // registry, so the CLI uninstall above cannot have touched it.
    statusline_remove(plugin, scope)?;
    Ok(Outcome::Removed)
}

// --- statusLine ---------------------------------------------------------------

/// CC's settings file for `scope`. User scope honors `CLAUDE_CONFIG_DIR` (the
/// override every isolated install uses, and the one CC itself reads) and falls
/// back to `~/.claude`; project scope is the project's own `.claude/settings.json`.
fn settings_file(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(cc_config_dir()?.join("settings.json")),
        Scope::Project { path } => Ok(path.join(".claude").join("settings.json")),
    }
}

fn cc_config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::home_dir()
        .map(|home| home.join(".claude"))
        .ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.claude".into()))
}

/// This host's status line rendered into CC's slot shape, paired with the bare
/// command string it carries; both have `${AGENTGEAR_CLIENT}` expanded to this
/// backend's own client id. `None` when the host declares none, which makes every
/// statusLine step below a no-op.
///
/// A blank command reads as "declares none" too. `StatusLineDecl::default()` carries
/// one, and rendering it would displace (and stash) the user's real status line in
/// exchange for a command that does nothing — a host bug that should cost them
/// nothing.
fn rendered_statusline(plugin: &Plugin) -> Option<(Value, String)> {
    let decl = plugin.statusline.as_ref()?;
    let command = expand_client(&decl.command, ClaudeBackend.id());
    if command.trim().is_empty() {
        return None;
    }
    Some((StatusLineDecl { command: command.clone(), ..decl.clone() }.to_value(), command))
}

/// Whether the slot's live value is one of OUR renderings — matched on the command
/// string, NOT on the whole object. Deliberately a different test from the one
/// [`statusline_state`] uses for convergence: any field drifting from what we render
/// is drift to repair, but only the command decides whose value it is.
///
/// Whole-value equality here would read our own earlier rendering as foreign the
/// moment a host release changes its padding. That misreading is not cosmetic:
/// reconcile would stash our command as "the user's original", destroying their real
/// value, and `compose` would then run this binary from inside itself on every turn.
///
/// Ceiling: a release that changes the COMMAND itself (renamed subcommand, new flag)
/// still reads as foreign, so it stashes its own old command and the user's value is
/// lost. The statusline module refuses to RUN a stash naming its own command, so the
/// worst case stays a missing row rather than re-entry. Upgrade path: write an
/// ownership key beside `command` — which needs CC's settings schema proven tolerant
/// of an unknown key first.
fn is_ours(existing: &Value, our_command: &str) -> bool {
    existing.get("command").and_then(Value::as_str) == Some(our_command)
}

/// Converge CC's single statusLine slot to the host's declaration, returning
/// whether settings.json changed. A foreign value already in the slot is stashed
/// verbatim into the stamp marker so `remove` can put it back.
fn statusline_reconcile(plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<bool> {
    let Some((ours, our_command)) = rendered_statusline(plugin) else {
        return Ok(false);
    };
    let path = settings_file(scope)?;

    // Stash BEFORE writing: the write is irreversible, so recording what it displaced
    // only afterwards loses the user's value outright when the marker write fails
    // (ENOSPC, EPERM) or the process dies between the two. An empty slot writes no
    // stash at all, so "user deletes our line, self_heal re-adds it" cannot erase
    // what they had before we ever wrote.
    if let Some(existing) = read_settings(&path)?.and_then(|root| root.get(STATUSLINE_KEY).cloned())
        && !is_ours(&existing, &our_command)
    {
        stamp::stash_statusline(plugin, scope, &desired.source, ClaudeBackend.id(), existing)?;
    }

    confedit::json_edit(&path, |root| {
        let obj = confedit::json_obj_at(root, &[]);
        if obj.get(STATUSLINE_KEY) != Some(&ours) {
            obj.insert(STATUSLINE_KEY.to_string(), ours.clone());
        }
        Ok(())
    })
}

/// Undo the slot write: restore the stashed original, or delete the key when there
/// was nothing to stash. Ownership is [`is_ours`] — the command string — so a user
/// who nudged only the padding on our line does not strand a command pointing at the
/// binary being uninstalled, while a genuinely foreign value is left exactly as it is.
///
/// An unparseable settings.json refuses the whole edit (`Error::Config`) rather than
/// clobbering it, so the stash is never consumed against a file that could not be read.
fn statusline_remove(plugin: &Plugin, scope: &Scope) -> Result<bool> {
    let Some((_, our_command)) = rendered_statusline(plugin) else {
        return Ok(false);
    };
    let stashed = stamp::read(plugin, scope, ClaudeBackend.id())?.and_then(|m| m.statusline_original);
    let path = settings_file(scope)?;
    confedit::json_edit(&path, |root| {
        let obj = confedit::json_obj_at(root, &[]);
        if !obj.get(STATUSLINE_KEY).is_some_and(|existing| is_ours(existing, &our_command)) {
            return Ok(());
        }
        match &stashed {
            Some(original) => obj.insert(STATUSLINE_KEY.to_string(), original.clone()),
            None => obj.remove(STATUSLINE_KEY),
        };
        Ok(())
    })
}

/// The statusLine surface's own state, or `None` when the host declares no status
/// line (contributing nothing to the probe). Convergence is whole-value equality, not
/// [`is_ours`]: our own rendering with a drifted `padding` is exactly the drift a
/// repair exists to fix. An unparseable settings.json reads as `Absent`; the reconcile
/// that follows refuses to clobber it and surfaces the parse error instead of silently
/// overwriting the user's file.
fn statusline_state(plugin: &Plugin, scope: &Scope) -> Result<Option<BackendState>> {
    let Some((ours, _)) = rendered_statusline(plugin) else {
        return Ok(None);
    };
    let root = read_settings(&settings_file(scope)?)?;
    Ok(Some(match root.as_ref().and_then(|r| r.get(STATUSLINE_KEY)) {
        None => BackendState::Absent,
        Some(existing) if *existing == ours => BackendState::Healthy,
        Some(_) => BackendState::NeedsRepair,
    }))
}

/// Parse a settings file for a read-only inspection: missing or unparseable both
/// read as "nothing to see", since neither is this function's to repair.
fn read_settings(path: &Path) -> Result<Option<Value>> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io { context: format!("reading {}", path.display()), source }),
    }
}

/// doctor's statusLine slice, or `None` when the host declares no status line.
/// A foreign owner is a Warn, not a Fail: CC's slot holds one value, so losing it
/// to another tool is a real (and user-visible) state, not a broken install.
pub(crate) fn statusline_check(plugin: &Plugin) -> Option<DoctorCheck> {
    let name = "status line installed";
    let (ours, _) = rendered_statusline(plugin)?;
    let path = match settings_file(&Scope::User) {
        Ok(path) => path,
        Err(e) => return Some(DoctorCheck { name, status: CheckStatus::Warn(format!("could not locate Claude Code's settings: {e}")) }),
    };
    let root = read_settings(&path).ok().flatten();
    Some(match root.as_ref().and_then(|r| r.get(STATUSLINE_KEY)) {
        Some(existing) if *existing == ours => {
            DoctorCheck { name, status: CheckStatus::Ok(format!("`{}` owns the statusLine slot", plugin.name)) }
        }
        Some(_) => DoctorCheck {
            name,
            status: CheckStatus::Warn(format!(
                "another status line owns `statusLine` in {}; the slot holds one value, so ours is not shown",
                path.display()
            )),
        },
        None => DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("no `statusLine` in {}", path.display()),
                fix: "run the host binary's `setup` (or `install`) subcommand".into(),
            },
        },
    })
}

#[cfg(test)]
#[path = "../../tests/unit/claude.rs"]
mod claude_tests;
