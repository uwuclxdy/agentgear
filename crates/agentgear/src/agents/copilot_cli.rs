//! The GitHub Copilot CLI backend: converge copilot's own plugin registry via
//! `copilot plugin marketplace add` + `copilot plugin install`, read back
//! through `copilot plugin list` (TEXT — copilot has no `--json`). It orchestrates
//! the supported CLI as its transaction boundary; it never forges copilot's on-disk
//! state.
//!
//! copilot ingests a plugin tree directly — live from a local marketplace's
//! source directory (>= 1.0.81), or cloned for a github marketplace — so every
//! tree surface (mcp, hooks, agents, skills) renders exactly as CC intends, with
//! no per-surface translation (the pre-1.0.81 backend hand-rendered copilot
//! config files). A local marketplace resolves through `current@<client>`, so a
//! tree change (same version or a version bump) reaches copilot by re-staging
//! that pointer alone — never an `uninstall`+`install` refresh.
//!
//! - **instructions**: the host's always-loaded guidance lands as a marker-tagged
//!   block in copilot's user-scope `copilot-instructions.md` — the one surface
//!   outside the ingested tree: the CLI lists that file as a `home`
//!   custom-instruction file and ships it into the system prompt with html comments
//!   intact. User scope only (see `instructions_file`); the marker id scopes
//!   ownership, so the user's lines and a rival plugin's block survive
//!   byte-identical, and the modular `instructions/` tree stays the user's own
//!   layout, never written.
//!
//! Constraints that shape the flow, all copilot-specific: version floor 1.0.92 (the
//! first whose `config` subcommand can clear the stale `enabledPlugins` key a
//! disable-only uninstall leaves); user-global installs with no `--scope`;
//! `marketplace update` refreshes catalogs but never re-points; `plugin list`
//! carries an `(enabled)`/`(disabled)` suffix. copilot's own `enabledPlugins.<id>`
//! record says what a `(disabled)` row means: the key `false` is a deliberate
//! disable (a hand `copilot plugin disable` or a disable-only `plugin uninstall`),
//! while no key is the `marketplace add` listing that precedes a first install
//! (measured 1.0.92, 1.0.104), a half-install self_heal converges. Probe reads
//! `Disabled` for either; self_heal leaves a key-`false` disable, converges a
//! keyless half-install at our pointer, and reads a listing registered elsewhere
//! as `Absent` (the user's own same-named marketplace, never ours) whatever its
//! `enabledPlugins` key or enabled state; an explicit `setup` runs `plugin
//! enable` first and fails loud on a foreign registration.
//!
//! All three sources (embedded/path/github) converge through the same
//! marketplace-add + `plugin install <plugin>@<marketplace>` path. **github cannot
//! pin a ref on copilot**: `owner/repo@ref` is parsed as a marketplace name and
//! `marketplace add` appends `.git` to the whole string, so only the bare `owner/repo`
//! is sent and copilot `git clone --depth 1` its DEFAULT BRANCH (live-verified
//! 1.0.71). agentgear's version-pin guarantee therefore cannot hold on
//! copilot+github — a present github install is treated as converged (probe
//! `Healthy`, reconcile `NoOp`) rather than churned toward the baked version. This is
//! a copilot CLI limitation, not an agentgear bug.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::agentsmd;
use super::report;
use super::{AgentBackend, BackendState};
use crate::cli::{CopilotCli, CopilotMarketplace, CopilotPlugin, MIN_COPILOT_VERSION, Output, copilot_meets_floor, version_lt};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};
use crate::materialize::{TreeSource, content_hash, materialize};
use crate::stamp;

pub(crate) struct CopilotCliBackend;

impl AgentBackend for CopilotCliBackend {
    fn id(&self) -> &'static str {
        "copilot-cli"
    }

    fn detect(&self) -> bool {
        // Native: the backend drives the `copilot` CLI, so a machine without it on
        // PATH cannot install regardless of any `~/.copilot` dir left behind.
        which::which("copilot").is_ok()
    }

    fn capabilities(&self) -> Capabilities {
        // Plugin-native like claude: copilot ingests the CC tree wholesale, so every
        // tree surface (mcp + hooks + commands + agents + skills) is served natively.
        // `instructions` is the one surface outside the tree: the shared tagged-block
        // renderer writes copilot's own user-scope `copilot-instructions.md`. User
        // scope only — copilot installs are user-global with no `--scope`.
        Capabilities {
            plugins: true,
            mcp: true,
            hooks: true,
            commands: true,
            agents: true,
            skills: true,
            instructions: true,
            scopes: &["user"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // CLI-based: copilot's registry is the source of truth, so scope never enters
        // the REGISTRY half (installs are user-global). `source` distinguishes only
        // github (unpinnable ref -> presence-only, never version-churn) from a
        // version-comparable embedded/path install; presence decides whether the
        // install is here, and the `(disabled)` suffix reads a present row as a
        // deliberate disable.
        let cli = CopilotCli::locate()?;
        let found = find_plugin(&cli, plugin)?;
        let Some(entry) = found.as_ref() else {
            // Absent settles it before the tree is read: a plugin this harness never had is
            // nothing to compare a tree against, and hashing one here would spend a session
            // start on a harness with nothing of ours in it.
            return Ok(BackendState::Absent);
        };
        // A marketplace registered under our name that this backend did not add is the
        // user's own: whatever its row says (enabled, disabled, keyed or keyless), it is
        // not our install, so probe reads Absent. self_heal then no-ops markerless or
        // clears a stale marker (never resurrect, never error); an explicit `setup`
        // refuses it in reconcile. Ours sits at our pointer (or its legacy plain
        // `current`), so it falls through to the state classify below.
        if matches!(marketplace_ownership(&cli, plugin, source)?, MarketplaceOwnership::Foreign(_)) {
            return Ok(BackendState::Absent);
        }
        let tree_current = tree_is_current(plugin, scope, staged_tree_hash(plugin, source).as_deref())?;
        let registry = classify(source, Some(entry), plugin.version, tree_current);
        // A frozen install — strictly newer than this binary — is left alone by
        // `present_action`, so no surface this binary would write may classify it
        // NeedsRepair: doing so spawns a heal every session for a no-op to throw away.
        // github never freezes (its version tracks a default branch), so it keeps the
        // fold: reconcile does write the block for it.
        if install_is_frozen(source, Some(entry), plugin.version) {
            return Ok(registry);
        }
        // The registry alone decides presence; the guidance block folds in as one more
        // surface, so a deleted or drifted block reads NeedsRepair and the heal's
        // `(marker present, Healthy)` row cannot no-op over it.
        let instructions = match instructions_file(scope)? {
            Some(path) => agentsmd::probe(&path, plugin)?,
            None => None,
        };
        Ok(report::compose([Some(registry), instructions].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        reconcile(plugin, desired, scope)
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
        remove(plugin, scope, source)
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        // The copilot-specific checks only; the doctor fan-out owns the shared
        // host-binary check (calling `doctor` here would recurse through `report`).
        DoctorReport::from_checks(report_checks(plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// copilot's config dir: a non-empty `COPILOT_HOME`, else `~/.copilot`. An empty
/// override reads as unset, the same reading every other harness-home override in
/// this crate takes (`CODEX_HOME`, `KIRO_HOME`, `KIMI_CODE_HOME`, …).
fn copilot_home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("COPILOT_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::home_dir()
        .map(|home| home.join(".copilot"))
        .ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.copilot".into()))
}

/// The user-scope `copilot-instructions.md` under copilot's config dir, the
/// always-loaded instructions channel the CLI lists as a `home` custom-instruction
/// file and ships into the system prompt with html comments intact. `None` at
/// project scope: copilot's repo-scope files (`<project>/.github/copilot-instructions.md`,
/// `<project>/AGENTS.md`, `<project>/CLAUDE.md`) are the user's repo files, never a
/// surface this backend writes.
fn instructions_file(scope: &Scope) -> Result<Option<PathBuf>> {
    match scope {
        Scope::User => Ok(Some(copilot_home()?.join("copilot-instructions.md"))),
        Scope::Project { .. } => Ok(None),
    }
}

// --- state reads -------------------------------------------------------------

fn find_plugin(cli: &CopilotCli, plugin: &Plugin) -> Result<Option<CopilotPlugin>> {
    Ok(cli.plugin_list(None)?.into_iter().find(|e| e.plugin == plugin.name && e.marketplace == plugin.marketplace))
}

fn marketplace_present(cli: &CopilotCli, name: &str) -> Result<bool> {
    Ok(cli.marketplace_list(None)?.iter().any(|m| m.name == name))
}

// --- mutating calls ----------------------------------------------------------

fn marketplace_add(cli: &CopilotCli, dir: &str) -> Result<()> {
    cli.run(&["plugin", "marketplace", "add", dir], None)?;
    Ok(())
}

fn plugin_install(cli: &CopilotCli, spec: &str) -> Result<()> {
    cli.run(&["plugin", "install", spec], None)?;
    Ok(())
}

/// `copilot plugin enable <id>`, the explicit-setup counterpart to a deliberate
/// `plugin disable`: self_heal never calls it, only an explicit install/update.
fn plugin_enable(cli: &CopilotCli, id: &str) -> Result<()> {
    cli.run(&["plugin", "enable", id], None)?;
    Ok(())
}

/// `copilot plugin uninstall`, treating copilot's `... is not installed` text as a
/// benign already-removed. Its exit code for that case is unconfirmed, so the text
/// is checked, not the code alone.
fn plugin_uninstall(cli: &CopilotCli, id: &str) -> Result<()> {
    let out = cli.run_capturing(&["plugin", "uninstall", id], None)?;
    if out.code == 0 {
        return Ok(());
    }
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if text.contains("is not installed") {
        return Ok(());
    }
    Err(Error::Cli { bin: "copilot", args: format!("plugin uninstall {id}"), code: out.code, stderr: text.trim().to_string() })
}

fn marketplace_remove(cli: &CopilotCli, name: &str) -> Result<()> {
    cli.run(&["plugin", "marketplace", "remove", name], None)?;
    Ok(())
}

/// `copilot config --rm enabledPlugins.<id>`, clearing the stale key the
/// disable-only uninstall leaves. Any nonzero exit fails the teardown: on a
/// config-capable copilot a leftover key naming our id is a removal we report.
fn config_rm(cli: &CopilotCli, plugin: &Plugin) -> Result<()> {
    let (key, _) = enabled_plugins_rm_key(plugin);
    cli.run(&["config", "--rm", &key], None)?;
    Ok(())
}

/// The stale `enabledPlugins.<id>` key a disable-only uninstall leaves, plus
/// whether copilot's dot-notation `config --rm` can address it (a `.` in the id
/// makes it unaddressable).
fn enabled_plugins_rm_key(plugin: &Plugin) -> (String, bool) {
    let id = plugin.id();
    (format!("enabledPlugins.{id}"), id.contains('.'))
}

/// What the stale `enabledPlugins.<id>` key needs. Split out of `remove` so the
/// dotted-id skip is testable without a dotted fixture plugin.
#[derive(Debug, PartialEq, Eq)]
enum StaleKey {
    /// No key names our id: nothing to clear.
    Absent,
    /// The key is clearable: `copilot config --rm` it.
    Clear,
    /// A dotted id: `copilot config --rm` cannot address the key, so name the
    /// leftover instead.
    Name,
}

fn stale_key_plan(plugin: &Plugin, present: bool) -> StaleKey {
    if !present {
        return StaleKey::Absent;
    }
    if enabled_plugins_rm_key(plugin).1 { StaleKey::Name } else { StaleKey::Clear }
}

/// The by-hand note `remove` prints for a present dotted key, which copilot's
/// dot-notation `config --rm` cannot address. Split out so the `Name` arm's
/// output is pinned by equality without a dotted fixture plugin.
fn stale_key_note(plugin: &Plugin) -> String {
    let (key, _) = enabled_plugins_rm_key(plugin);
    format!("agentgear: `{key}` still names our id and `copilot config --rm` cannot clear a dotted key; remove it by hand if it is yours")
}

// --- reconcile ---------------------------------------------------------------

fn reconcile(plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
    let cli = CopilotCli::locate()?;
    let id = plugin.id();

    // The tree this pass would stage, hashed before anything is written: the hash
    // is the same-version-edit signal `tree_is_current` reads off the stamp
    // marker, which the registry's version column cannot see.
    let staged = staged_tree_hash(plugin, &desired.source);
    let tree_current = tree_is_current(plugin, scope, staged.as_deref())?;

    // The registry half first, exactly as claude splits it: `PresentAction::Frozen`
    // returns with nothing to do.
    let registry = match find_plugin(&cli, plugin)? {
        None => {
            // Absent: full install. Every source (embedded/path/github) flows through
            // marketplace-add + `plugin install <plugin>@<marketplace>`; a github
            // source registers a marketplace under the name in the repo's root
            // marketplace.json, which is `plugin.marketplace`, so the id keyed on here
            // matches.
            // A same-named marketplace this backend did not add is the user's own on
            // every source: an unlisted row goes straight to `plugin install`, which
            // would install the user's plugin as ours, so ask ownership first.
            if let MarketplaceOwnership::Foreign(reason) = marketplace_ownership(&cli, plugin, &desired.source)? {
                return Err(Error::CopilotMarketplaceForeign(format!(
                    "{} is not installed, and marketplace `{}` is registered but not this host's to manage: {reason}; run `copilot plugin marketplace remove {}` (it uninstalls every plugin from that marketplace), then re-run setup",
                    id, plugin.marketplace, plugin.marketplace
                )));
            }
            cli.ensure_min_version()?;
            ensure_marketplace(&cli, plugin, &desired.source)?;
            plugin_install(&cli, &id)?;
            verify_present(&cli, plugin)?;
            record_staged(plugin, desired, scope, staged.as_deref())?;
            Outcome::Installed
        }
        Some(entry) => {
            // A same-named marketplace this backend did not add is the user's own on
            // every source: refuse it before any arm can enable, re-stage, or no-op
            // over it (a no-op return would stamp our marker on their row).
            if let MarketplaceOwnership::Foreign(reason) = marketplace_ownership(&cli, plugin, &desired.source)? {
                if !matches!(desired.source, Source::GitHub { .. }) {
                    ensure_served_from_pointer(&cli, plugin)?;
                }
                return Err(Error::CopilotMarketplaceForeign(format!(
                    "{} is installed from marketplace `{}`, which is not this host's to manage: {reason}; run `copilot plugin marketplace remove {}` (it uninstalls every plugin from that marketplace), then re-run setup",
                    id, plugin.marketplace, plugin.marketplace
                )));
            }
            // copilot's own `enabledPlugins.<id>` record decides what a
            // `(disabled)` row means, never the marker: `false` is a deliberate
            // disable (or a disable-only uninstall), which self_heal leaves and an
            // explicit setup re-enables; no key is the `marketplace add` listing
            // that precedes a first install, which self_heal converges.
            let mut re_enabled = false;
            if entry.disabled {
                cli.ensure_min_version()?;
                if !desired.reenable && enabled_plugins_key(&cli, plugin)? == EnabledPluginsKey::Disabled {
                    return Ok(Outcome::NoOp);
                }
                // The registration check must run before the enable: a failed
                // setup/heal over a foreign or legacy registration must not leave
                // the row enabled on a stale tree.
                if !matches!(desired.source, Source::GitHub { .. }) {
                    ensure_served_from_pointer(&cli, plugin)?;
                }
                plugin_enable(&cli, &id)?;
                // Exit 0 alone does not prove the enable: real copilot exits 0
                // with a warning when nothing was enabled. Re-read the row and
                // require it to read enabled.
                if find_plugin(&cli, plugin)?.is_none_or(|e| e.disabled) {
                    return Err(Error::Verify(format!("{id} still disabled after `copilot plugin enable`")));
                }
                re_enabled = true;
            }
            match present_action(&desired.source, entry.version.as_deref(), plugin.version, tree_current) {
                // A newer binary owns this install; nothing here is ours to touch.
                PresentAction::Frozen => return Ok(if re_enabled { Outcome::Repaired } else { Outcome::NoOp }),
                PresentAction::NoOp => {
                    if re_enabled {
                        Outcome::Repaired
                    } else {
                        Outcome::NoOp
                    }
                }
                PresentAction::Update => {
                    // A local source is served live through the pointer, so the
                    // version bump reaches copilot by re-staging the tree alone
                    // (its version column re-reads the tree's plugin.json); github
                    // never reaches this arm.
                    cli.ensure_min_version()?;
                    ensure_marketplace(&cli, plugin, &desired.source)?;
                    ensure_served_from_pointer(&cli, plugin)?;
                    verify_present(&cli, plugin)?;
                    record_staged(plugin, desired, scope, staged.as_deref())?;
                    Outcome::Updated { from: entry.version.clone(), to: plugin.version.to_string() }
                }
                PresentAction::Refresh => {
                    // Same version, different tree: copilot serves a local-marketplace
                    // plugin live from the pointer, so re-staging the pointer re-hands
                    // the bytes — never an uninstall+install, which would disable the
                    // row and read like a user's disable.
                    cli.ensure_min_version()?;
                    ensure_marketplace(&cli, plugin, &desired.source)?;
                    ensure_served_from_pointer(&cli, plugin)?;
                    verify_present(&cli, plugin)?;
                    record_staged(plugin, desired, scope, staged.as_deref())?;
                    Outcome::Repaired
                }
            }
        }
    };

    // The guidance block lands only after the registry op above succeeded (or read
    // converged): a failed install returns before this, so it can never leave
    // guidance behind for a plugin copilot never got. A block this pass repaired is
    // itself a repair, so a registry `NoOp` does not swallow it.
    if let Some(path) = instructions_file(scope)?
        && agentsmd::reconcile(&path, plugin)?
    {
        return Ok(match registry {
            Outcome::NoOp => Outcome::Repaired,
            other => other,
        });
    }
    Ok(registry)
}

/// Ensure our marketplace is registered, add-if-absent. Embedded/path add the
/// client-scoped `current@copilot-cli` dir; github adds the bare `repo` (copilot
/// clones its default branch). `marketplace update` refreshes catalogs but never
/// re-points, so flipping `current@copilot-cli` in place re-points it across
/// versions AND across tree edits, and a re-materialize needs no re-add: the
/// registered marketplace resolves through the pointer to whatever was staged
/// last, and copilot serves that tree live, so re-staging alone re-hands it.
///
/// Ceiling: an install predating client-scoping sits on a plain `current` this code
/// no longer writes, and `marketplace update` never re-points an entry (a
/// `marketplace remove` deletes, never re-points), so such an install must be
/// reinstalled (`uninstall` + `setup`) to migrate onto the client-scoped staging.
/// Fresh installs are unaffected.
fn ensure_marketplace(cli: &CopilotCli, plugin: &Plugin, source: &Source) -> Result<()> {
    // Client-scope the materialization under this backend's own id, so copilot and CC
    // never collide on the shared data root (each bakes its own `${AGENTGEAR_CLIENT}`).
    let client = CopilotCliBackend.id();
    let add_source = match source {
        Source::Embedded => materialize(plugin, TreeSource::Blob(plugin.blob()), client)?.display().to_string(),
        // A path source materializes its on-disk tree the same way embedded does.
        Source::Path(p) => materialize(plugin, TreeSource::Dir(p), client)?.display().to_string(),
        Source::GitHub { repo, ref_ } => github_marketplace_source(repo, ref_),
    };
    if !marketplace_present(cli, plugin.marketplace)? {
        marketplace_add(cli, &add_source)?;
    }
    Ok(())
}

/// A re-stage reaches copilot only through a marketplace registered at this
/// host's `current@copilot-cli` pointer. `marketplace add` refuses an existing
/// name, so any other registration (a legacy plain `current`, a hand-added repo
/// or dir) keeps serving its own tree and must fail loud rather than record a
/// convergence copilot never sees.
fn ensure_served_from_pointer(cli: &CopilotCli, plugin: &Plugin) -> Result<()> {
    let ours = crate::host::current_pointer(plugin.name, CopilotCliBackend.id())?;
    let Some(entry) = cli.marketplace_list(None)?.into_iter().find(|m| m.name == plugin.marketplace) else {
        return Ok(());
    };
    if let Some((problem, fix)) = served_from_pointer_mismatch(&entry, plugin, &ours) {
        return Err(Error::CopilotMarketplaceForeign(format!("{problem}; {fix}")));
    }
    Ok(())
}

/// `None` when the registered entry IS this host's pointer; else the rendered
/// problem and fix, shared by reconcile (an error) and doctor (a failing check)
/// so both name the same registration and the same remedy.
fn served_from_pointer_mismatch(entry: &CopilotMarketplace, plugin: &Plugin, ours: &Path) -> Option<(String, String)> {
    if entry.path.as_deref().is_some_and(|path| paths_same(Path::new(path), ours)) {
        return None;
    }
    let registered = entry.path.as_deref().or(entry.github.as_deref()).unwrap_or("<unknown source>");
    let problem = format!(
        "{} is served from marketplace `{}` registered at `{registered}`, not this host's `{}`, so copilot never sees the re-staged tree",
        plugin.id(),
        plugin.marketplace,
        ours.display()
    );
    Some((problem, served_from_pointer_fix(entry, plugin, ours)))
}

/// The fix for a marketplace registered anywhere but this host's pointer. The
/// legacy plain `current` under our data root needs the host's own uninstall
/// (which drops the legacy registration); a foreign registration needs
/// `marketplace remove`.
fn served_from_pointer_fix(entry: &CopilotMarketplace, plugin: &Plugin, ours: &Path) -> String {
    if entry.path.as_deref().is_some_and(|path| is_legacy_current(path, ours)) {
        "run the host binary's `uninstall` subcommand (it drops the legacy registration), then re-run setup".to_string()
    } else {
        format!(
            "run `copilot plugin marketplace remove {}` (it uninstalls every plugin from that marketplace), then re-run setup",
            plugin.marketplace
        )
    }
}

/// The `marketplace add` source string for a github source. copilot `git clone
/// --depth 1` the repo's DEFAULT BRANCH and cannot pin a ref: `owner/repo@ref` is
/// parsed as a marketplace name, and `marketplace add` appends `.git` to the whole
/// string (both live-verified 1.0.71), so `_ref` is DROPPED and the bare `repo` is
/// sent. copilot registers it under the name in the repo's root
/// `.claude-plugin/marketplace.json` — exactly `plugin.marketplace`, so the install
/// (`<plugin>@<plugin.marketplace>`), probe, and remove all key on the same id.
fn github_marketplace_source(repo: &str, _ref: &str) -> String {
    repo.to_string()
}

fn verify_present(cli: &CopilotCli, plugin: &Plugin) -> Result<()> {
    if find_plugin(cli, plugin)?.is_some() {
        Ok(())
    } else {
        Err(Error::Verify(format!("{} absent from `copilot plugin list` after the operation", plugin.id())))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PresentAction {
    /// Converged: nothing to do in the registry (a github install is ours, just
    /// unpinnable).
    NoOp,
    Update,
    /// Same version, different tree: copilot serves a local-marketplace plugin live
    /// from the pointer, so re-staging the pointer re-hands the bytes.
    Refresh,
    /// A strictly-newer install: a newer binary owns it. claude's
    /// `RegistryOutcome::Frozen`, split out of `NoOp` because the two need different
    /// handling.
    Frozen,
}

/// Present-plugin reconcile decision. github can't pin a ref (copilot tracks the
/// default branch), so its installed version is unrelated to the baked one — a
/// present github install is always converged (`NoOp`), never churning on `plugin
/// update`. Other sources are monotonic: update only toward a strictly-newer embedded
/// version; a strictly-newer install (a coexisting newer binary) is `Frozen`, so two
/// binaries never downgrade each other and neither takes the other's slot. An
/// unparseable or missing installed version is neither older nor newer, so it stays
/// `NoOp` — converged, slot included.
///
/// `tree_current` is the last term, and only reached at a version that is neither
/// stale nor newer: a version bump re-stages the tree anyway, and a newer install
/// belongs to a binary whose tree is not ours to replace.
fn present_action(source: &Source, installed: Option<&str>, embedded: &str, tree_current: bool) -> PresentAction {
    match source {
        Source::GitHub { .. } => PresentAction::NoOp,
        _ if version_lt(installed, embedded) => PresentAction::Update,
        _ if installed.is_some_and(|v| version_lt(Some(embedded), v)) => PresentAction::Frozen,
        _ if !tree_current => PresentAction::Refresh,
        _ => PresentAction::NoOp,
    }
}

/// The hash of the tree this reconcile would stage for copilot.
///
/// `None` covers both "there is no local tree" (github, where copilot tracks the repo's
/// default branch) and "the local tree cannot be read right now" — a `--path` checkout
/// that moved, a reaped worktree, a zero-embed binary. Both mean no drift is DETECTABLE,
/// never that the tree drifted: a healthy install whose source went away used to
/// converge to a silent no-op, and turning that into a hard failure would red every
/// session-start heal from then on. A source that must be read to converge still fails
/// loudly the moment `ensure_marketplace` materializes from it.
fn staged_tree_hash(plugin: &Plugin, source: &Source) -> Option<String> {
    let client = CopilotCliBackend.id();
    match source {
        Source::Embedded => content_hash(TreeSource::Blob(plugin.blob()), client).ok(),
        Source::Path(p) => content_hash(TreeSource::Dir(p), client).ok(),
        Source::GitHub { .. } => None,
    }
}

/// Whether copilot already holds the tree `staged` names, read off this agent's own
/// stamp marker. Unknown content converges rather than assuming freshness: a github
/// source has no tree to compare (always current), while a marker that is absent or
/// predates the record leaves the tree copilot serves unaccounted for, and the version
/// comparison can never account for it either.
fn tree_is_current(plugin: &Plugin, scope: &Scope, staged: Option<&str>) -> Result<bool> {
    let Some(staged) = staged else {
        return Ok(true);
    };
    let marker = stamp::read(plugin, scope, CopilotCliBackend.id())?;
    Ok(marker.and_then(|m| m.tree_hash).is_some_and(|recorded| recorded == staged))
}

/// Record the tree copilot now holds, after the staging that handed it over succeeded.
/// A github source records no hash — it has no local tree, and overwriting an earlier
/// local record would hide the drift of a host switching back.
fn record_staged(plugin: &Plugin, desired: &Desired, scope: &Scope, staged: Option<&str>) -> Result<()> {
    stamp::record_converged(plugin, scope, &desired.source, CopilotCliBackend.id(), staged)
}

/// Classify presence + version into the self_heal state. A `(disabled)` row is a
/// deliberate disable and outranks everything else, so self_heal never re-enables
/// it. github can't pin a ref, so a present github install is `Healthy`
/// regardless of the baked version (a mismatch is the default branch drifting, not
/// a repairable break — avoids churn). Other sources compare monotonic version.
///
/// `tree_current` is the one thing the registry read cannot show: a same-version
/// tree edit leaves both the entry and its version correct. Without it here,
/// self_heal's `(marker present, Healthy)` row no-ops forever and a box converges
/// only on an explicit `setup`.
fn classify(source: &Source, entry: Option<&CopilotPlugin>, embedded: &str, tree_current: bool) -> BackendState {
    match entry {
        None => BackendState::Absent,
        Some(e) if e.disabled => BackendState::Disabled,
        Some(_) if matches!(source, Source::GitHub { .. }) => BackendState::Healthy,
        Some(e) if version_lt(e.version.as_deref(), embedded) => BackendState::NeedsRepair,
        // Monotonic outranks the tree term, exactly as `present_action`'s `Frozen` does:
        // a strictly-newer install holds a newer binary's tree, which never matches our
        // hash, so classifying it NeedsRepair would spawn a reconcile every session for
        // the `Frozen` no-op to throw away.
        Some(_) if install_is_frozen(source, entry, embedded) => BackendState::Healthy,
        Some(_) if !tree_current => BackendState::NeedsRepair,
        Some(_) => BackendState::Healthy,
    }
}

/// Is the registered install strictly newer than this binary? `present_action` returns
/// `Frozen` for exactly this state, leaving the install to the binary that owns it.
fn install_is_frozen(source: &Source, entry: Option<&CopilotPlugin>, embedded: &str) -> bool {
    // `present_action` matches the github arm first, so a github install is never
    // Frozen however its tracked version compares.
    !matches!(source, Source::GitHub { .. }) && entry.and_then(|e| e.version.as_deref()).is_some_and(|v| version_lt(Some(embedded), v))
}

// --- remove ------------------------------------------------------------------

/// What teardown may do to the registered marketplace under our name.
enum MarketplaceAction {
    /// Ours and unshared: `marketplace remove` is safe to run.
    Remove,
    /// Present but not ours to drop (a foreign source).
    Keep,
    /// Ours, but other plugins from it are still installed; `marketplace remove`
    /// would drop them too, so it is kept and the siblings are named.
    Shared(Vec<String>),
    /// No registered marketplace with our name: nothing to remove.
    Absent,
}

/// Whether the marketplace registered under our name is the one this backend
/// added: the list + foreign-reason half of [`marketplace_action`], for the paths
/// whose question is ownership alone (probe, reconcile, doctor) and must not pay
/// for the sibling computation only uninstall needs.
enum MarketplaceOwnership {
    /// No registered marketplace with our name.
    Absent,
    /// Registered and the entry this backend added.
    Ours,
    /// Present but not ours; the string is the rendered reason.
    Foreign(String),
}

/// Why our marketplace stayed registered after the uninstall half, carried to the
/// post-check that errors when our plugin is still listed.
enum Kept {
    /// Removed or absent: nothing of ours stayed behind.
    None,
    /// Ours but shared: kept because sibling plugins still use it.
    Shared(Vec<String>),
}

/// The sibling clause of the shared-marketplace error, plural-correct:
/// "`a` still uses it" for one, "`a`, `b` still use it" for several.
fn shared_sibling_clause(siblings: &[String]) -> String {
    let list = siblings.join("`, `");
    let verb = if siblings.len() == 1 { "uses" } else { "use" };
    format!("`{list}` still {verb} it")
}

/// Whether a registered local marketplace path is this host's own pointer: the
/// `current@<client>` path, anything that canonicalizes to it (a symlink alias),
/// or the legacy plain `current` under our data root (this host's pre-scoping
/// layout).
fn local_marketplace_is_ours(path: &str, ours: &Path) -> bool {
    paths_same(Path::new(path), ours) || is_legacy_current(path, ours)
}

/// Whether a registered local path is this host's pre-scoping plain `current`
/// under our data root — the layout an install from before client-scoping left.
fn is_legacy_current(path: &str, ours: &Path) -> bool {
    ours.parent().is_some_and(|root| Path::new(path) == root.join("current"))
}

/// True when two paths are the same, comparing canonicalized paths and falling
/// back to the literal when either target is gone.
fn paths_same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// Decide whether the marketplace registered under our name is safe to remove.
/// Ours means its source matches what this backend added; even then it is kept
/// when another plugin from it is still installed, because copilot's
/// `marketplace remove` drops every plugin installed from it (>= 1.0.81).
fn marketplace_action(cli: &CopilotCli, plugin: &Plugin, source: &Source) -> Result<MarketplaceAction> {
    match marketplace_ownership(cli, plugin, source)? {
        MarketplaceOwnership::Foreign(_) => Ok(MarketplaceAction::Keep),
        MarketplaceOwnership::Absent => Ok(MarketplaceAction::Absent),
        MarketplaceOwnership::Ours => {
            let installed = enabled_plugins_keys(cli)?;
            let siblings = marketplace_siblings(&cli.plugin_list(None)?, plugin, &installed);
            if !siblings.is_empty() { Ok(MarketplaceAction::Shared(siblings)) } else { Ok(MarketplaceAction::Remove) }
        }
    }
}

/// The list + foreign-reason half of [`marketplace_action`]: whether the
/// registered entry is the one this backend added, without computing the sibling
/// set only uninstall needs.
fn marketplace_ownership(cli: &CopilotCli, plugin: &Plugin, source: &Source) -> Result<MarketplaceOwnership> {
    let Some(entry) = cli.marketplace_list(None)?.into_iter().find(|m| m.name == plugin.marketplace) else {
        return Ok(MarketplaceOwnership::Absent);
    };
    match marketplace_foreign_reason(&entry, plugin, source)? {
        Some(reason) => Ok(MarketplaceOwnership::Foreign(reason)),
        None => Ok(MarketplaceOwnership::Ours),
    }
}

/// `None` when the registered entry is the one this backend added, else the
/// rendered reason it is foreign. Local sources key on the materialized
/// `current@<client>` pointer (or its canonicalized/legacy forms); a github
/// source must match the `(GitHub: <repo>)` detail exactly.
fn marketplace_foreign_reason(entry: &CopilotMarketplace, plugin: &Plugin, source: &Source) -> Result<Option<String>> {
    match source {
        Source::Embedded | Source::Path(_) => {
            let ours = crate::host::current_pointer(plugin.name, CopilotCliBackend.id())?;
            Ok(match entry.path.as_deref() {
                Some(path) if local_marketplace_is_ours(path, &ours) => None,
                Some(path) => Some(format!("it is registered at `{path}` but this host's is `{}`", ours.display())),
                None => Some("it is registered without a local path".to_string()),
            })
        }
        Source::GitHub { repo, .. } => {
            // copilot echoes the add string verbatim, so exact compare is the only
            // signal; a user's own exact-spelling pre-add of the same repo reads as
            // ours and is dropped with it. claude.rs matches by name alone.
            if entry.github.as_deref() == Some(repo) {
                Ok(None)
            } else {
                Ok(Some(format!(
                    "it is registered as `(GitHub: {})` but this host's is `{repo}`",
                    entry.github.as_deref().unwrap_or("<none>")
                )))
            }
        }
    }
}

/// The other plugins from our marketplace actually installed, named as their
/// `<plugin>@<marketplace>` ids. `marketplace remove` drops every plugin
/// installed from it, so a shared marketplace is kept — but only an
/// `enabledPlugins` key marks an install: a merely-listed sibling (`marketplace
/// add` writes every marketplace plugin as a `(disabled)` row before any
/// install) is not one.
fn marketplace_siblings(rows: &[CopilotPlugin], plugin: &Plugin, installed: &HashSet<String>) -> Vec<String> {
    rows.iter()
        .filter(|e| e.marketplace == plugin.marketplace && e.plugin != plugin.name)
        .map(|e| format!("{}@{}", e.plugin, e.marketplace))
        .filter(|id| installed.contains(id))
        .collect()
}

/// The set of plugin ids copilot's `enabledPlugins` group records — exactly the
/// plugins copilot counts as installed, whatever their enabled state. A key's
/// presence separates an install from a mere `marketplace add` listing, which
/// writes no key (measured 1.0.104).
fn enabled_plugins_keys(cli: &CopilotCli) -> Result<HashSet<String>> {
    let out = cli.run_capturing(&["config", "enabledPlugins"], None)?;
    Ok(enabled_plugins_map(&out)?.keys().cloned().collect())
}

/// What copilot's own `enabledPlugins.<id>` record says about our plugin, read
/// through `copilot config enabledPlugins` (a group read that creates no file,
/// unlike `--rm`).
#[derive(Debug, PartialEq, Eq)]
enum EnabledPluginsKey {
    /// No key names our id: `marketplace add` lists a plugin `(disabled)` before
    /// a first install, so nothing ever recorded it enabled.
    Absent,
    /// The key reads `false`: a deliberate disable, or a disable-only uninstall.
    Disabled,
    /// The key reads `true` (or any present non-`false` value): copilot recorded
    /// it enabled.
    Enabled,
}

fn enabled_plugins_key(cli: &CopilotCli, plugin: &Plugin) -> Result<EnabledPluginsKey> {
    let out = cli.run_capturing(&["config", "enabledPlugins"], None)?;
    enabled_plugins_key_from(&out, plugin)
}

/// The parse of one `config enabledPlugins` read. An absent group exits 1 with no
/// output (measured 1.0.92, 1.0.104) and reads as "key absent"; any other nonzero
/// exit or an unparseable stdout fails loud, never folding to "key absent".
fn enabled_plugins_key_from(out: &Output, plugin: &Plugin) -> Result<EnabledPluginsKey> {
    Ok(match enabled_plugins_map(out)?.get(plugin.id().as_str()) {
        None => EnabledPluginsKey::Absent,
        Some(serde_json::Value::Bool(false)) => EnabledPluginsKey::Disabled,
        Some(_) => EnabledPluginsKey::Enabled,
    })
}

/// The `config enabledPlugins` group read as a map, with the same error policy as
/// [`enabled_plugins_key_from`]: an absent group (rc 1, no output) is an empty
/// map; any other nonzero exit or unparseable stdout fails loud.
fn enabled_plugins_map(out: &Output) -> Result<serde_json::Map<String, serde_json::Value>> {
    if out.code != 0 {
        if out.stdout.is_empty() && out.stderr.is_empty() {
            return Ok(serde_json::Map::new());
        }
        let mut detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if detail.is_empty() {
            detail = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        return Err(Error::Cli { bin: "copilot", args: "config enabledPlugins".to_string(), code: out.code, stderr: detail });
    }
    serde_json::from_slice(&out.stdout).map_err(|source| Error::Json { what: "`copilot config enabledPlugins`".to_string(), source })
}

/// Strip our always-loaded guidance block first (so no teardown — even one refused
/// by the version floor — strands it), then take back only the state this backend
/// added: `plugin uninstall` (at >= 1.0.81 that only disables), then `marketplace
/// remove` when the marketplace is ours and unshared (which drops the still-listed
/// row), then `config --rm` for the stale `enabledPlugins` key the disable-only
/// path leaves. A foreign same-named marketplace is the user's own and is left
/// alone (with its row) — only our guidance block and marker are dropped — so
/// that path reports `Removed` when our guidance was stripped and `NoOp` otherwise;
/// on the ours path, `Removed` is reported only when a post-check confirms our
/// plugin is gone from `plugin list`.
fn remove(plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
    let cli = CopilotCli::locate()?;

    // The block is ours by marker, whatever the registry says: strip it on every
    // teardown, before the version gate, so a below-floor copilot — or a strip
    // that failed after a successful one — can never strand always-loaded
    // guidance in the user's config.
    let mut changed = false;
    if let Some(path) = instructions_file(scope)? {
        changed |= agentsmd::remove(&path, plugin)?;
    }

    let version = cli.ensure_min_version()?;

    // Decide ownership before the disable-only `plugin uninstall`: a foreign
    // same-named marketplace is not ours to drop, and the plugin installed from
    // it is the user's own install, so `remove` leaves both alone (a disable-only
    // uninstall would leave their row `(disabled)`).
    let action = marketplace_action(&cli, plugin, source)?;
    if let MarketplaceAction::Keep = &action {
        // Not ours: the user's foreign marketplace and its row stay untouched, but
        // our guidance block (stripped above) and marker are ours to drop. The
        // marker is cleared by the uninstall fan-out after this returns Ok.
        return Ok(if changed { Outcome::Removed } else { Outcome::NoOp });
    }

    if find_plugin(&cli, plugin)?.is_some() {
        plugin_uninstall(&cli, &plugin.id())?;
        changed = true;
    }

    let kept = match action {
        MarketplaceAction::Remove => {
            marketplace_remove(&cli, plugin.marketplace)?;
            changed = true;
            Kept::None
        }
        MarketplaceAction::Shared(siblings) => Kept::Shared(siblings),
        MarketplaceAction::Absent => Kept::None,
        MarketplaceAction::Keep => unreachable!("foreign marketplace returned above"),
    };

    // `config --rm` writes a lock file even when there is nothing to clear, so it
    // runs only when our key is present. A dotted id cannot be addressed through
    // copilot's dot-notation config key at all; name the leftover instead.
    // A kept (shared) marketplace leaves our row listed `(disabled)`; its `false`
    // key is copilot's record of that disable, not a stale leftover, so clear it
    // only when the marketplace is gone (a later markerless self-heal must read
    // the disable, never a keyless half-install).
    let key_present = !matches!(kept, Kept::Shared(_)) && enabled_plugins_key(&cli, plugin)? != EnabledPluginsKey::Absent;
    match stale_key_plan(plugin, key_present) {
        StaleKey::Absent => {}
        StaleKey::Name => {
            eprintln!("{}", stale_key_note(plugin));
        }
        StaleKey::Clear => {
            config_rm(&cli, plugin)?;
            changed = true;
        }
    }

    // `plugin uninstall` alone disables (keeps the row) on copilot >= 1.0.81, so an
    // exit-0 uninstall is not proof of removal: re-read the registry and fail rather
    // than report `Removed` over a plugin copilot still lists.
    if find_plugin(&cli, plugin)?.is_some() {
        let id = plugin.id();
        return Err(match kept {
            Kept::Shared(siblings) => Error::CopilotMarketplaceForeign(format!(
                "{id} was uninstalled but marketplace `{}` stays because {}; `copilot plugin marketplace remove` would uninstall {} too",
                plugin.marketplace,
                shared_sibling_clause(&siblings),
                if siblings.len() == 1 { "it" } else { "them" }
            )),
            Kept::None => Error::Verify(format!("{id} still listed by `copilot plugin list` after uninstall (copilot {version})")),
        });
    }
    Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
}

// --- report ------------------------------------------------------------------

fn report_checks(plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let cli = match CopilotCli::locate() {
        Ok(cli) => cli,
        Err(_) => {
            checks.push(DoctorCheck {
                name: "copilot on PATH",
                status: CheckStatus::Fail {
                    problem: "`copilot` not found on PATH".into(),
                    fix: "install it with `npm install -g @github/copilot`".into(),
                },
            });
            return checks;
        }
    };
    checks.push(check_version(&cli));
    check_registered(&cli, plugin, source, &mut checks);
    if !matches!(source, Source::GitHub { .. }) {
        check_served_from_pointer(&cli, plugin, &mut checks);
    }
    check_stale_enabled_key(&cli, plugin, &mut checks);
    if plugin.instructions.is_some() {
        checks.push(agentsmd::guidance_check(instructions_file(&Scope::User), plugin));
    }
    checks
}

/// A dotted plugin id leaves an `enabledPlugins.<id>` key no copilot subcommand
/// can clear (`config --rm` cannot descend past the dot), so doctor names it.
/// The key is copilot's own record of the installed plugin (a healthy install
/// reads `true`), so it is stale only once our row is gone — the leftover a
/// dotted-id uninstall leaves.
fn check_stale_enabled_key(cli: &CopilotCli, plugin: &Plugin, checks: &mut Vec<DoctorCheck>) {
    if !plugin.id().contains('.') {
        return;
    }
    let name = "stale enabledPlugins key";
    let listed = match find_plugin(cli, plugin) {
        Ok(row) => row.is_some(),
        Err(e) => {
            checks.push(DoctorCheck { name, status: CheckStatus::Warn(format!("could not read `copilot plugin list`: {e}")) });
            return;
        }
    };
    match enabled_plugins_key(cli, plugin) {
        Ok(key) => {
            if let Some(problem) = stale_dotted_key(plugin, listed, key) {
                checks.push(DoctorCheck { name, status: CheckStatus::Warn(problem) });
            }
        }
        Err(e) => {
            checks.push(DoctorCheck { name, status: CheckStatus::Warn(format!("could not read `copilot config enabledPlugins`: {e}")) })
        }
    }
}

/// The stale dotted-key decision, split out so the warn/not-warn pair is
/// unit-pinned without a dotted fixture host. `listed` is whether our row is
/// still in `plugin list`; a healthy install lists it, so the key is copilot's
/// own record of the install, never stale.
fn stale_dotted_key(plugin: &Plugin, listed: bool, key: EnabledPluginsKey) -> Option<String> {
    if !plugin.id().contains('.') || listed {
        return None;
    }
    match key {
        EnabledPluginsKey::Absent => None,
        EnabledPluginsKey::Disabled | EnabledPluginsKey::Enabled => {
            Some(format!("`enabledPlugins.{}` still names our id; `copilot config --rm` cannot clear a dotted key", plugin.id()))
        }
    }
}

fn check_version(cli: &CopilotCli) -> DoctorCheck {
    let name = "copilot version";
    let raw = match cli.raw_version() {
        Ok(v) => v,
        Err(e) => return DoctorCheck { name, status: CheckStatus::Warn(format!("could not read `copilot --version`: {e}")) },
    };
    match copilot_meets_floor(&raw) {
        Some(false) => DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("`copilot` {raw} is below {MIN_COPILOT_VERSION}, required for plugin management"),
                fix: "upgrade with `copilot update`".into(),
            },
        },
        Some(true) => DoctorCheck { name, status: CheckStatus::Ok(raw) },
        None => DoctorCheck { name, status: CheckStatus::Warn(format!("could not parse version {raw:?}; proceeding")) },
    }
}

fn check_registered(cli: &CopilotCli, plugin: &Plugin, source: &Source, checks: &mut Vec<DoctorCheck>) {
    let name = "plugin registered";
    match cli.plugin_list(None) {
        Err(e) => checks.push(DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("`copilot plugin list` failed: {e}"),
                fix: "re-run `copilot plugin list` and report the output".into(),
            },
        }),
        Ok(entries) => match entries.iter().find(|e| e.plugin == plugin.name && e.marketplace == plugin.marketplace) {
            Some(entry) => match marketplace_ownership(cli, plugin, source) {
                // A foreign marketplace registration is the user's own row, never
                // ours: name it instead of counting it registered here.
                Ok(MarketplaceOwnership::Foreign(reason)) => checks.push(DoctorCheck {
                    name,
                    status: CheckStatus::Fail {
                        problem: format!("{} is installed from a foreign marketplace: {reason}", plugin.id()),
                        fix: format!(
                            "run `copilot plugin marketplace remove {}` (it uninstalls every plugin from that marketplace), then re-run setup",
                            plugin.marketplace
                        ),
                    },
                }),
                Ok(_) => {
                    let version = entry.version.clone().unwrap_or_else(|| "?".into());
                    let state = if entry.disabled { "disabled" } else { "enabled" };
                    checks.push(DoctorCheck { name, status: CheckStatus::Ok(format!("{} v{version} ({state})", plugin.id())) });
                }
                Err(e) => checks.push(DoctorCheck { name, status: CheckStatus::Warn(format!("could not read the marketplace registration: {e}")) }),
            },
            None => checks.push(DoctorCheck {
                name,
                status: CheckStatus::Fail {
                    problem: format!("{} is not installed", plugin.id()),
                    fix: "run the host binary's `setup` (or `install`) subcommand".into(),
                },
            }),
        },
    }
}

/// Whether the registered marketplace (when present) is the one copilot serves
/// our tree from — this host's `current@copilot-cli` pointer. A foreign
/// registration is already named by the `plugin registered` check, so this check
/// covers the one state ownership still reads as ours but the pointer compare
/// does not: the legacy plain `current` (which must fail, not read all-green).
fn check_served_from_pointer(cli: &CopilotCli, plugin: &Plugin, checks: &mut Vec<DoctorCheck>) {
    let name = "marketplace served from pointer";
    let ours = match crate::host::current_pointer(plugin.name, CopilotCliBackend.id()) {
        Ok(path) => path,
        Err(e) => {
            checks.push(DoctorCheck { name, status: CheckStatus::Warn(format!("could not resolve the materialized pointer path: {e}")) });
            return;
        }
    };
    let entries = match cli.marketplace_list(None) {
        Ok(entries) => entries,
        Err(e) => {
            checks.push(DoctorCheck { name, status: CheckStatus::Warn(format!("could not read `copilot plugin marketplace list`: {e}")) });
            return;
        }
    };
    let Some(entry) = entries.into_iter().find(|m| m.name == plugin.marketplace) else {
        return; // nothing registered to compare; the `plugin registered` check covers it
    };
    // A registration at a path this host never added is foreign and already named
    // by the `plugin registered` check, so it must not fail here a second time.
    if !entry.path.as_deref().is_some_and(|p| local_marketplace_is_ours(p, &ours)) {
        return;
    }
    match served_from_pointer_mismatch(&entry, plugin, &ours) {
        None => checks
            .push(DoctorCheck { name, status: CheckStatus::Ok(format!("`{}` registered at `{}`", plugin.marketplace, ours.display())) }),
        Some((problem, fix)) => checks.push(DoctorCheck { name, status: CheckStatus::Fail { problem, fix } }),
    }
}

#[cfg(test)]
#[path = "../../tests/unit/copilot_cli.rs"]
mod copilot_cli_tests;
