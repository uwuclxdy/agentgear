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

use super::{AgentBackend, BackendState};
use crate::cli::{CopilotCli, CopilotPlugin, MIN_COPILOT_VERSION, copilot_meets_floor, version_lt};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};
use crate::materialize::{TreeSource, materialize};

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
        // scope only — copilot installs are user-global with no `--scope`.
        Capabilities { plugins: true, mcp: true, hooks: true, commands: true, agents: true, skills: true, scopes: &["user"] }
    }

    fn probe(&self, plugin: &Plugin, _scope: &Scope, source: &Source) -> Result<BackendState> {
        // CLI-based: copilot's registry is the source of truth, so scope (user-global)
        // never enters the probe. `source` distinguishes only github (unpinnable ref
        // -> presence-only, never version-churn) from a version-comparable
        // embedded/path install; `plugin list` has no install-path/enabled column, so
        // there is no `Disabled` / files-gone state.
        let cli = CopilotCli::locate()?;
        Ok(classify(source, find_plugin(&cli, plugin)?.as_ref(), plugin.version))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, _scope: &Scope) -> Result<Outcome> {
        reconcile(plugin, desired)
    }

    fn remove(&self, plugin: &Plugin, _scope: &Scope) -> Result<Outcome> {
        remove(plugin)
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

fn reconcile(plugin: &Plugin, desired: &Desired) -> Result<Outcome> {
    let cli = CopilotCli::locate()?;
    let id = plugin.id();

    let Some(entry) = find_plugin(&cli, plugin)? else {
        // Absent: full install. Every source (embedded/path/github) flows through
        // marketplace-add + `plugin install <plugin>@<marketplace>`; a github source
        // registers a marketplace under the name in the repo's root marketplace.json,
        // which is `plugin.marketplace`, so the id keyed on here matches.
        cli.ensure_min_version()?;
        ensure_marketplace(&cli, plugin, &desired.source)?;
        plugin_install(&cli, &id)?;
        verify_present(&cli, plugin)?;
        return Ok(Outcome::Installed);
    };

    match present_action(&desired.source, entry.version.as_deref(), plugin.version) {
        PresentAction::NoOp => Ok(Outcome::NoOp),
        PresentAction::Update => {
            cli.ensure_min_version()?;
            ensure_marketplace(&cli, plugin, &desired.source)?;
            plugin_update(&cli, &id)?;
            verify_present(&cli, plugin)?;
            Ok(Outcome::Updated { from: entry.version.clone(), to: plugin.version.to_string() })
        }
    }
}

/// Ensure our marketplace is registered, add-if-absent. Embedded/path materialize a
/// local tree and add its `current` dir; github adds the bare `repo` (copilot clones
/// its default branch). copilot has no `marketplace update`; the `current` pointer is
/// stable across versions, so a re-materialize needs no re-add and `plugin update`
/// re-reads the refreshed tree.
fn ensure_marketplace(cli: &CopilotCli, plugin: &Plugin, source: &Source) -> Result<()> {
    let add_source = match source {
        Source::Embedded => materialize(plugin, TreeSource::Blob(plugin.blob()))?.display().to_string(),
        // A path source materializes its on-disk tree the same way embedded does.
        Source::Path(p) => materialize(plugin, TreeSource::Dir(p))?.display().to_string(),
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
    NoOp,
    Update,
}

/// Present-plugin reconcile decision. github can't pin a ref (copilot tracks the
/// default branch), so its installed version is unrelated to the baked one — a
/// present github install is always converged (`NoOp`), never churning on `plugin
/// update`. Other sources are monotonic: update only toward a strictly-newer embedded
/// version; a same-or-newer install (e.g. a coexisting newer binary) is left
/// untouched, so two binaries never downgrade each other.
fn present_action(source: &Source, installed: Option<&str>, embedded: &str) -> PresentAction {
    match source {
        Source::GitHub { .. } => PresentAction::NoOp,
        _ if version_lt(installed, embedded) => PresentAction::Update,
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
/// marketplace stays registered (a harmless dangling entry pointing at `current`).
fn remove(plugin: &Plugin) -> Result<Outcome> {
    let cli = CopilotCli::locate()?;
    if find_plugin(&cli, plugin)?.is_none() {
        return Ok(Outcome::NoOp);
    }
    plugin_uninstall(&cli, &plugin.id())?;
    Ok(Outcome::Removed)
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
