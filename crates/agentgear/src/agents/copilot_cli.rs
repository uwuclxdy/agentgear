//! The GitHub Copilot CLI backend: converge copilot's own plugin registry via
//! `copilot plugin marketplace add` + `copilot plugin install`/`update`, read back
//! through `copilot plugin list` (TEXT — copilot has no `--json`). It orchestrates
//! the supported CLI as its transaction boundary; it never forges copilot's on-disk
//! state.
//!
//! copilot 1.0.71+ copies the CC tree **verbatim** into
//! `~/.copilot/installed-plugins/<marketplace>/<plugin>/`, so every surface (mcp,
//! hooks, agents, skills) renders exactly as CC intends — there is no per-surface
//! translation here (the pre-1.0.71 backend hand-rendered copilot config files;
//! `docs/harness/copilot-cli.md` covers the switch to native ingestion).
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
//!
//! One exception to "no per-surface translation": copilot's `statusLine` slot lives in
//! the user's own `$COPILOT_HOME/settings.json`, not in any plugin tree, so a host
//! that declares a status line gets it written here through the shared
//! [`super::statuslinejson`] lifecycle. That module owns the whole slot contract
//! (stash-before-write, restore-on-remove, command-string ownership); this backend
//! supplies only copilot's settings path, key path, and value shape. USER SCOPE ONLY,
//! matching the only scope this backend has.

use std::path::PathBuf;

use super::statuslinejson::{self, SlotShape};
use super::{AgentBackend, BackendState};
use crate::cli::{CopilotCli, CopilotPlugin, MIN_COPILOT_VERSION, copilot_meets_floor, version_lt};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};
use crate::materialize::{TreeSource, materialize};

/// copilot's settings key path for the status-line slot: one root-level key holding a
/// single object, never an array.
const STATUSLINE_SLOT: &[&str] = &["statusLine"];

/// CC's body, unchanged. copilot 1.0.75 documents and implements exactly
/// `{type?: "command", command, padding?}`: `type` is optional and its renderer reads
/// only `command` + `padding` (`docs/research/statusline-survey.md` §4), so
/// `TypedCommand` already covers the whole of what this harness stores — nothing to
/// carry, no row cap, no variant of its own.
///
/// The renderer swallows every error and renders empty for a blank or non-string
/// command, so a wrong key path here fails completely silently. Verify against the
/// survey, never by watching a copilot session.
const STATUSLINE_SHAPE: SlotShape = SlotShape::typed_command();

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
        // surface (mcp + hooks + commands + agents + skills) is served natively. User
        // scope only — copilot installs are user-global with no `--scope`, and the
        // status-line slot's repo-overridability could not be enumerated (the
        // governance allow-list is built inside copilot's Rust `runtime.node`).
        // `statusline` is true and NOT implied by `plugins`: the slot lives in the
        // user's own `settings.json`, outside every tree copilot ingests.
        Capabilities {
            plugins: true,
            mcp: true,
            hooks: true,
            commands: true,
            agents: true,
            skills: true,
            instructions: false,
            statusline: true,
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
        let registry = classify(source, find_plugin(&cli, plugin)?.as_ref(), plugin.version);
        if matches!(registry, BackendState::Absent) {
            return Ok(BackendState::Absent);
        }
        // The registry alone decides presence. A statusLine of ours still sitting in
        // `settings.json` after a manual `copilot plugin uninstall` must not read as
        // "present but drifted", or self_heal would resurrect a deliberate uninstall
        // (the Absent arm above already returned). Once the plugin IS registered, a
        // missing or foreign statusLine is drift like any other surface.
        //
        // No `ensure_statusline_resolves` hoist here, deliberately: probe never mutates
        // anything (only reads), so an unresolvable config dir cannot strand a partial
        // registry write the way `reconcile`/`remove` can. Hoisting it above the
        // `Absent` early-return would also turn every self-heal poll of a plugin that
        // was never installed into a hard failure purely from a broken env var, with
        // nothing to protect and nothing to repair.
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

    /// copilot's statusLine slot is our only write outside its plugin registry, so a
    /// plugin the user removed by hand leaves our command behind with nothing left to
    /// restore it once the marker (and its stash) goes. Project scope reaches here too
    /// — this backend declares user scope only, so an uninstall at project scope is a
    /// `ScopeUnsupported` skip that still calls `forget` — and is a no-op, because
    /// `statusline_target` serves no project scope.
    fn forget(&self, plugin: &Plugin, scope: &Scope) -> Result<()> {
        statusline_remove(plugin, scope).map(|_| ())
    }

    fn report(&self, plugin: &Plugin, _source: &Source) -> DoctorReport {
        // The copilot-specific checks only; the doctor fan-out owns the shared
        // host-binary check (calling `doctor` here would recurse through `report`).
        DoctorReport::from_checks(report_checks(plugin))
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
    ensure_statusline_resolves(plugin, scope)?;
    let cli = CopilotCli::locate()?;
    let id = plugin.id();

    // The registry half first, exactly as claude splits it: `PresentAction::Frozen`
    // returns before the slot is touched, every other arm falls through to converge it.
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
            Outcome::Installed
        }
        Some(entry) => match present_action(&desired.source, entry.version.as_deref(), plugin.version) {
            // A newer binary owns this install, so its slot is theirs too: skip the
            // slot write entirely rather than writing over another owner's value.
            PresentAction::Frozen => return Ok(Outcome::NoOp),
            PresentAction::NoOp => Outcome::NoOp,
            PresentAction::Update => {
                cli.ensure_min_version()?;
                ensure_marketplace(&cli, plugin, &desired.source)?;
                plugin_update(&cli, &id)?;
                verify_present(&cli, plugin)?;
                Outcome::Updated { from: entry.version.clone(), to: plugin.version.to_string() }
            }
        },
    };

    // A drifted statusLine behind an otherwise-converged registry is still a repair;
    // any real registry change already outranks it.
    let changed = statusline_reconcile(plugin, desired, scope)?;
    Ok(match (registry, changed) {
        (Outcome::NoOp, true) => Outcome::Repaired,
        (outcome, _) => outcome,
    })
}

/// Ensure our marketplace is registered, add-if-absent. Embedded/path add the
/// client-scoped `current@copilot-cli` dir; github adds the bare `repo` (copilot
/// clones its default branch). copilot has no `marketplace update`, but flipping
/// `current@copilot-cli` in place re-points it across versions, so a re-materialize
/// needs no re-add and `plugin update` re-reads the refreshed tree.
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
    /// Converged: nothing to do in the registry, and the status-line slot still
    /// converges (a github install is ours, just unpinnable).
    NoOp,
    Update,
    /// A strictly-newer install: a newer binary owns it, the slot included. claude's
    /// `RegistryOutcome::Frozen`, split out of `NoOp` because the two need different
    /// slot handling — writing our command into an install another binary owns would
    /// point its status line at the wrong binary.
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
fn present_action(source: &Source, installed: Option<&str>, embedded: &str) -> PresentAction {
    match source {
        Source::GitHub { .. } => PresentAction::NoOp,
        _ if version_lt(installed, embedded) => PresentAction::Update,
        _ if installed.is_some_and(|v| version_lt(Some(embedded), v)) => PresentAction::Frozen,
        _ => PresentAction::NoOp,
    }
}

/// Classify presence + version into the self_heal state. github can't pin a ref, so
/// a present github install is `Healthy` regardless of the baked version (a mismatch
/// is the default branch drifting, not a repairable break — avoids churn). Other
/// sources compare monotonic version. copilot's `plugin list` carries no install-path
/// or enabled column, so there is no `Disabled` / files-gone state.
fn classify(source: &Source, entry: Option<&CopilotPlugin>, embedded: &str) -> BackendState {
    match entry {
        None => BackendState::Absent,
        Some(_) if matches!(source, Source::GitHub { .. }) => BackendState::Healthy,
        Some(e) if version_lt(e.version.as_deref(), embedded) => BackendState::NeedsRepair,
        Some(_) => BackendState::Healthy,
    }
}

// --- remove ------------------------------------------------------------------

/// Uninstall our plugin. copilot exposes no `marketplace remove`, so the local
/// marketplace stays registered (a harmless dangling entry pointing at `current@copilot-cli`).
///
/// The statusLine slot lives in the user's own `settings.json`, outside the plugin
/// registry, so the CLI uninstall cannot have touched it — and exact-remove for a slot
/// means RESTORE: put back what our write displaced. Either half changing something is
/// a `Removed`; neither is the `NoOp` this backend's teardown contract promises.
fn remove(plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
    ensure_statusline_resolves(plugin, scope)?;
    let cli = CopilotCli::locate()?;
    let mut changed = false;
    if find_plugin(&cli, plugin)?.is_some() {
        plugin_uninstall(&cli, &plugin.id())?;
        changed = true;
    }
    changed |= statusline_remove(plugin, scope)?;
    Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
}

// --- statusLine ---------------------------------------------------------------

/// copilot's config dir: `$COPILOT_HOME` when set and non-empty, else `~/.copilot`.
///
/// copilot's own bundle resolves `process.env.COPILOT_HOME ?? join(homedir(),
/// ".copilot")` (1.0.75 bundle, `docs/research/statusline-survey.md` §4): `??` is
/// nullish-only, so copilot reads `COPILOT_HOME=""` as the config dir itself and
/// resolves `settings.json` against the CWD, not `~/.copilot`. A silent fallback here
/// would therefore write a settings file copilot itself never reads behind a green
/// doctor, so the empty case is rejected instead through the shared
/// [`super::non_empty_config_dir`]. Same idiom as claude's `cc_config_dir`.
fn copilot_home() -> Result<PathBuf> {
    if let Some(dir) = super::config_dir_override("COPILOT_HOME")? {
        return Ok(dir);
    }
    dirs::home_dir()
        .map(|home| home.join(".copilot"))
        .ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.copilot".into()))
}

/// The user settings file copilot migrated its status line into out of the legacy
/// `config.json`.
fn statusline_file() -> Result<PathBuf> {
    Ok(copilot_home()?.join("settings.json"))
}

/// The settings file the slot lifecycle writes, or `None` when there is nothing to
/// write: the host declares no status line, or the scope is project.
///
/// USER SCOPE ONLY. copilot's repo-scope settings files exist
/// (`.github/copilot/settings.json`), but whether `statusLine` is repo-overridable
/// could not be enumerated — the governance allow-list is built inside copilot's Rust
/// `runtime.node` (`docs/research/statusline-survey.md` §4).
///
/// The scope guard is load-bearing, not cosmetic. `statuslinejson::remove` keys the
/// stash on `(plugin, scope, client)`, so a project-scope call would read an empty
/// marker, still see our command as `is_ours`, and DELETE the slot — losing the user's
/// original, which is stashed under the user-scope marker.
fn statusline_target(plugin: &Plugin, scope: &Scope) -> Result<Option<PathBuf>> {
    let Scope::User = scope else {
        return Ok(None);
    };
    statuslinejson::target(plugin, CopilotCliBackend.id(), STATUSLINE_SHAPE, statusline_file)
}

/// Resolve the statusLine settings path before `reconcile`/`remove` run any `copilot`
/// CLI call, so an empty `COPILOT_HOME` refuses the whole operation up front instead
/// of letting the registry mutation (marketplace add/install/update/uninstall) run to
/// completion and only failing afterward on the slot write — which would leave the
/// plugin installed or removed in copilot's own registry behind a `Failed` status.
/// `cli.rs` never scrubs `COPILOT_HOME` from the child env, so the real CLI would
/// otherwise perform its own cwd-relative write before we ever got a chance to reject.
///
/// A no-op for a host that declares no status line: `statusline_target`'s own gate
/// already skips the config-dir lookup in that case (and for project scope), so such a
/// host keeps converging normally under an empty override — nothing IT writes is
/// misplaced, since copilot resolves its own config dir independently of ours.
fn ensure_statusline_resolves(plugin: &Plugin, scope: &Scope) -> Result<()> {
    statusline_target(plugin, scope)?;
    Ok(())
}

fn statusline_reconcile(plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<bool> {
    let Some(path) = statusline_target(plugin, scope)? else {
        return Ok(false);
    };
    statuslinejson::reconcile(&path, STATUSLINE_SLOT, plugin, &desired.source, scope, CopilotCliBackend.id(), STATUSLINE_SHAPE)
}

fn statusline_remove(plugin: &Plugin, scope: &Scope) -> Result<bool> {
    let Some(path) = statusline_target(plugin, scope)? else {
        return Ok(false);
    };
    statuslinejson::remove(&path, STATUSLINE_SLOT, plugin, scope, CopilotCliBackend.id(), STATUSLINE_SHAPE)
}

fn statusline_state(plugin: &Plugin, scope: &Scope) -> Result<Option<BackendState>> {
    let Some(path) = statusline_target(plugin, scope)? else {
        return Ok(None);
    };
    statuslinejson::state(&path, STATUSLINE_SLOT, plugin, CopilotCliBackend.id(), STATUSLINE_SHAPE)
}

// --- report ------------------------------------------------------------------

/// No `ensure_statusline_resolves` hoist here, deliberately: `report_checks` builds a
/// `Vec<DoctorCheck>` and never propagates an `Err` (every failure becomes its own
/// check), and every CLI call below is a read (`plugin list`, `--version`), never a
/// mutation, so an unresolvable config dir has no partial state to strand. The
/// `statusline_check` call below already surfaces it as its own check (a `Fail` for
/// an empty override, see `statuslinejson::check`).
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
    // Absent entirely for a host that declares no status line. User scope, matching the
    // only scope this backend (and the surface) has.
    checks.extend(statuslinejson::check(
        statusline_file(),
        STATUSLINE_SLOT,
        plugin,
        CopilotCliBackend.id(),
        STATUSLINE_SHAPE,
        "GitHub Copilot CLI",
    ));
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
