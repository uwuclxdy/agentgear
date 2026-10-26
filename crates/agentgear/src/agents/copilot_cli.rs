//! The GitHub Copilot CLI backend: converge copilot's own plugin registry via
//! `copilot plugin marketplace add` + `copilot plugin install`/`update`, read back
//! through `copilot plugin list` (TEXT — copilot has no `--json`). It orchestrates
//! the supported CLI as its transaction boundary; it never forges copilot's on-disk
//! state.
//!
//! copilot 1.0.71+ copies the CC tree **verbatim** into
//! `~/.copilot/installed-plugins/<marketplace>/<plugin>/`, so every tree surface
//! (mcp, hooks, agents, skills) renders exactly as CC intends — there is no
//! per-surface translation (the pre-1.0.71 backend hand-rendered copilot config
//! files; `docs/harness/copilot-cli.md` covers the switch to native ingestion).
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
//! Constraints that shape the flow, all copilot-specific (see
//! `docs/research/verify-copilot-cli.md`): version floor 1.0.71 (the `plugin`
//! lifecycle did not exist at 1.0.70); user-global installs with no `--scope`; no
//! `plugin enable`/`disable`; no `marketplace update`; `plugin list` carries only a
//! version column (no install-path/enabled), so probe is presence + monotonic
//! version only.
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

use std::path::PathBuf;

use super::agentsmd;
use super::report;
use super::{AgentBackend, BackendState};
use crate::cli::{CopilotCli, CopilotPlugin, MIN_COPILOT_VERSION, copilot_meets_floor, version_lt};
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
        // version-comparable embedded/path install; `plugin list` has no
        // install-path/enabled column, so there is no `Disabled` / files-gone state.
        let cli = CopilotCli::locate()?;
        let entry = find_plugin(&cli, plugin)?;
        // Absent settles it before the tree is read: a plugin this harness never had is
        // nothing to compare a tree against, and hashing one here would spend a session
        // start on a harness with nothing of ours in it.
        if entry.is_none() {
            return Ok(BackendState::Absent);
        }
        let tree_current = tree_is_current(plugin, scope, staged_tree_hash(plugin, source).as_deref())?;
        let registry = classify(source, entry.as_ref(), plugin.version, tree_current);
        // A frozen install — strictly newer than this binary — is left alone by
        // `present_action`, so no surface this binary would write may classify it
        // NeedsRepair: doing so spawns a heal every session for a no-op to throw away.
        if install_is_frozen(entry.as_ref(), plugin.version) {
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

    fn remove(&self, plugin: &Plugin, scope: &Scope, _source: &Source) -> Result<Outcome> {
        remove(plugin, scope)
    }

    fn report(&self, plugin: &Plugin, _source: &Source) -> DoctorReport {
        // The copilot-specific checks only; the doctor fan-out owns the shared
        // host-binary check (calling `doctor` here would recurse through `report`).
        DoctorReport::from_checks(report_checks(plugin))
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
/// project scope: copilot's repo-scope files (`.github/copilot-instructions.md`,
/// `AGENTS.md`, `CLAUDE.md`) are the user's repo files, never a surface this
/// backend writes.
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

fn plugin_update(cli: &CopilotCli, id: &str) -> Result<()> {
    cli.run(&["plugin", "update", id], None)?;
    Ok(())
}

/// `copilot plugin uninstall`, treating copilot's `... is not installed` text as a
/// benign already-removed. Its exit code for that case is unconfirmed, so the text
/// is checked, not the code alone (`docs/research/verify-copilot-cli.md`).
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

// --- reconcile ---------------------------------------------------------------

fn reconcile(plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
    let cli = CopilotCli::locate()?;
    let id = plugin.id();

    // The tree this pass would stage, hashed before anything is written: copilot copies
    // it into `~/.copilot/installed-plugins/<mkt>/<plugin>/` at install time and re-reads
    // THAT COPY every session, never the staged tree it was made from, so an edit at an
    // unchanged version reaches copilot only when the copy is rewritten.
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
            cli.ensure_min_version()?;
            ensure_marketplace(&cli, plugin, &desired.source)?;
            plugin_install(&cli, &id)?;
            verify_present(&cli, plugin)?;
            record_staged(plugin, desired, scope, staged.as_deref())?;
            Outcome::Installed
        }
        Some(entry) => match present_action(&desired.source, entry.version.as_deref(), plugin.version, tree_current) {
            // A newer binary owns this install; nothing here is ours to touch.
            PresentAction::Frozen => return Ok(Outcome::NoOp),
            PresentAction::NoOp => Outcome::NoOp,
            PresentAction::Update => {
                cli.ensure_min_version()?;
                ensure_marketplace(&cli, plugin, &desired.source)?;
                plugin_update(&cli, &id)?;
                verify_present(&cli, plugin)?;
                record_staged(plugin, desired, scope, staged.as_deref())?;
                Outcome::Updated { from: entry.version.clone(), to: plugin.version.to_string() }
            }
            PresentAction::Refresh => {
                // copilot's install copy is not version-keyed (one dir per plugin), so
                // uninstall + install is what replaces it; `plugin update` at an
                // unchanged version has nothing to compare and is not proven to re-copy.
                cli.ensure_min_version()?;
                ensure_marketplace(&cli, plugin, &desired.source)?;
                // Marked before the uninstall, never after: a failed `plugin install` or
                // a SessionStart hook killed between the two calls otherwise leaves a
                // marker beside an absent plugin, which self_heal reads as the user's own
                // uninstall and forgets for good.
                stamp::begin_reinstall(plugin, scope, &desired.source, CopilotCliBackend.id())?;
                plugin_uninstall(&cli, &id)?;
                plugin_install(&cli, &id)?;
                verify_present(&cli, plugin)?;
                record_staged(plugin, desired, scope, staged.as_deref())?;
                Outcome::Repaired
            }
        },
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
/// clones its default branch). copilot has no `marketplace update`, but flipping
/// `current@copilot-cli` in place re-points it across versions AND across tree edits,
/// so a re-materialize needs no re-add: the registered marketplace resolves through the
/// pointer to whatever was staged last. Handing that tree to the plugin registry is a
/// separate step (`PresentAction::Update`/`Refresh`), since copilot's install copy is
/// written once and re-read from there on, never refreshed from the staged tree.
///
/// Ceiling: an install predating client-scoping sits on a plain `current` this code
/// no longer writes, and copilot exposes no marketplace update/remove to re-point it,
/// so such an install must be reinstalled (`uninstall` + `setup`) to migrate onto the
/// client-scoped staging. Fresh installs are unaffected.
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
    /// Same version, different tree: copilot holds bytes this binary no longer ships,
    /// at a version that will never bump. Only a reinstall replaces its install copy.
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
/// stale nor newer: a version bump re-copies the tree anyway, and a newer install
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
/// predates the record leaves what copilot copied unaccounted for, and the version
/// comparison can never account for it either.
fn tree_is_current(plugin: &Plugin, scope: &Scope, staged: Option<&str>) -> Result<bool> {
    let Some(staged) = staged else {
        return Ok(true);
    };
    let marker = stamp::read(plugin, scope, CopilotCliBackend.id())?;
    Ok(marker.and_then(|m| m.tree_hash).is_some_and(|recorded| recorded == staged))
}

/// Record the tree copilot now holds, after the call that handed it over succeeded, and
/// end any reinstall this pass began. A github source records no hash — it has no local
/// tree, and overwriting an earlier local record would hide the drift of a host
/// switching back — but it still ends the reinstall.
fn record_staged(plugin: &Plugin, desired: &Desired, scope: &Scope, staged: Option<&str>) -> Result<()> {
    stamp::record_converged(plugin, scope, &desired.source, CopilotCliBackend.id(), staged)
}

/// Classify presence + version into the self_heal state. github can't pin a ref, so
/// a present github install is `Healthy` regardless of the baked version (a mismatch
/// is the default branch drifting, not a repairable break — avoids churn). Other
/// sources compare monotonic version. copilot's `plugin list` carries no install-path
/// or enabled column, so there is no `Disabled` / files-gone state.
///
/// `tree_current` is the one thing the registry read cannot show: copilot's install
/// copy is not version-keyed, so an edited tree at an unchanged version leaves both the
/// entry and its version correct. Without it here, self_heal's `(marker present,
/// Healthy)` row no-ops forever and a box converges only on an explicit `setup`.
fn classify(source: &Source, entry: Option<&CopilotPlugin>, embedded: &str, tree_current: bool) -> BackendState {
    match entry {
        None => BackendState::Absent,
        Some(_) if matches!(source, Source::GitHub { .. }) => BackendState::Healthy,
        Some(e) if version_lt(e.version.as_deref(), embedded) => BackendState::NeedsRepair,
        // Monotonic outranks the tree term, exactly as `present_action`'s `Frozen` does:
        // a strictly-newer install holds a newer binary's tree, which never matches our
        // hash, so classifying it NeedsRepair would spawn a reconcile every session for
        // the `Frozen` no-op to throw away.
        Some(_) if install_is_frozen(entry, embedded) => BackendState::Healthy,
        Some(_) if !tree_current => BackendState::NeedsRepair,
        Some(_) => BackendState::Healthy,
    }
}

/// Is the registered install strictly newer than this binary? `present_action` returns
/// `Frozen` for exactly this state, leaving the install to the binary that owns it.
fn install_is_frozen(entry: Option<&CopilotPlugin>, embedded: &str) -> bool {
    entry.and_then(|e| e.version.as_deref()).is_some_and(|v| version_lt(Some(embedded), v))
}

// --- remove ------------------------------------------------------------------

/// Uninstall our plugin. copilot exposes no `marketplace remove`, so the local
/// marketplace stays registered (a harmless dangling entry pointing at `current@copilot-cli`).
fn remove(plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
    let cli = CopilotCli::locate()?;
    let mut changed = false;
    if find_plugin(&cli, plugin)?.is_some() {
        plugin_uninstall(&cli, &plugin.id())?;
        changed = true;
    }
    // The block is ours by marker, whatever the registry says: strip it on every
    // teardown, so a by-hand `copilot plugin uninstall` — or a strip that failed after a
    // successful one — cannot strand always-loaded guidance in the user's config.
    if let Some(path) = instructions_file(scope)? {
        changed |= agentsmd::remove(&path, plugin)?;
    }
    Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
}

// --- report ------------------------------------------------------------------

fn report_checks(plugin: &Plugin) -> Vec<DoctorCheck> {
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
    check_registered(&cli, plugin, &mut checks);
    if plugin.instructions.is_some() {
        checks.push(agentsmd::guidance_check(instructions_file(&Scope::User), plugin));
    }
    checks
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

fn check_registered(cli: &CopilotCli, plugin: &Plugin, checks: &mut Vec<DoctorCheck>) {
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
            Some(entry) => {
                let version = entry.version.clone().unwrap_or_else(|| "?".into());
                checks.push(DoctorCheck { name, status: CheckStatus::Ok(format!("{} v{version}", plugin.id())) });
            }
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

#[cfg(test)]
#[path = "../../tests/unit/copilot_cli.rs"]
mod copilot_cli_tests;
