//! The Claude Code backend: converge the `claude plugin` registry to a desired
//! state via marketplace-ensure + install/update, read back through `list --json`.
//! It orchestrates the supported CLI; it never forges CC's on-disk state.
//!
//! One exception to "no config file": CC's `statusLine` slot lives in the user's
//! own `settings.json`, not in a plugin tree, so a host that declares a status line
//! gets it written here through the shared [`super::statuslinejson`] lifecycle. That
//! module owns the whole slot contract (stash-before-write, restore-on-remove,
//! command-string ownership); this backend supplies only CC's settings path, key
//! path, and value shape.

use std::path::{Path, PathBuf};

use super::statuslinejson::{self, SlotShape};
use super::{AgentBackend, BackendState};
use crate::cli::{ClaudeCli, version_lt};
use crate::doctor::{DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};
use crate::manifest::{MarketplaceEntry, PluginEntry};
use crate::materialize::{TreeSource, materialize};

/// CC's settings key path for the status-line slot: one top-level key holding a
/// single object (`{"type":"command","command":…}`), never an array.
const STATUSLINE_SLOT: &[&str] = &["statusLine"];

/// CC's own slot body, which qwen-code then copied verbatim.
const STATUSLINE_SHAPE: SlotShape = SlotShape::typed_command();

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

/// CC's settings file for the slot lifecycle, or `None` when the host declares no
/// status line. Resolved through the shared [`statuslinejson::target`] guard so a
/// declaration-free host never pays for — or fails on — a config-dir lookup it has
/// no use for.
fn statusline_target(plugin: &Plugin, scope: &Scope) -> Result<Option<PathBuf>> {
    statuslinejson::target(plugin, ClaudeBackend.id(), STATUSLINE_SHAPE, || settings_file(scope))
}

fn statusline_reconcile(plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<bool> {
    let Some(path) = statusline_target(plugin, scope)? else {
        return Ok(false);
    };
    statuslinejson::reconcile(&path, STATUSLINE_SLOT, plugin, &desired.source, scope, ClaudeBackend.id(), STATUSLINE_SHAPE)
}

fn statusline_remove(plugin: &Plugin, scope: &Scope) -> Result<bool> {
    let Some(path) = statusline_target(plugin, scope)? else {
        return Ok(false);
    };
    statuslinejson::remove(&path, STATUSLINE_SLOT, plugin, scope, ClaudeBackend.id(), STATUSLINE_SHAPE)
}

fn statusline_state(plugin: &Plugin, scope: &Scope) -> Result<Option<BackendState>> {
    let Some(path) = statusline_target(plugin, scope)? else {
        return Ok(None);
    };
    statuslinejson::state(&path, STATUSLINE_SLOT, plugin, ClaudeBackend.id(), STATUSLINE_SHAPE)
}

/// doctor's statusLine slice, or `None` when the host declares no status line.
pub(crate) fn statusline_check(plugin: &Plugin) -> Option<DoctorCheck> {
    statuslinejson::check(settings_file(&Scope::User), STATUSLINE_SLOT, plugin, ClaudeBackend.id(), STATUSLINE_SHAPE, "Claude Code")
}

#[cfg(test)]
#[path = "../../tests/unit/claude.rs"]
mod claude_tests;
