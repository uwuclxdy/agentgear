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
        // Plugin-native like claude: copilot ingests the CC tree wholesale, covering
        // mcp + hooks natively. User scope only — copilot installs are user-global
        // with no `--scope`.
        Capabilities { plugins: true, mcp: true, hooks: true, scopes: &["user"] }
    }

    fn probe(&self, plugin: &Plugin, _scope: &Scope, _source: &Source) -> Result<BackendState> {
        // CLI-based: copilot's registry is the source of truth, so neither scope
        // (user-global) nor the resolved `source` enters the probe. `plugin list`
        // has no install-path/enabled column, so state is presence + monotonic
        // version only (no `Disabled`, no files-gone break to detect).
        let cli = CopilotCli::locate()?;
        Ok(classify(find_plugin(&cli, plugin)?.as_ref(), plugin.version))
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

    // GitHub has no local tree to materialize + register as a marketplace, so it
    // takes copilot's direct-install path (best-effort, see `github_install`).
    if let Source::GitHub { repo, ref_ } = &desired.source {
        return github_install(&cli, repo, ref_);
    }

    let id = plugin.id();
    let Some(entry) = find_plugin(&cli, plugin)? else {
        // Absent: full install.
        cli.ensure_min_version()?;
        ensure_marketplace(&cli, plugin, &desired.source)?;
        plugin_install(&cli, &id)?;
        verify_present(&cli, plugin)?;
        return Ok(Outcome::Installed);
    };

    match present_action(entry.version.as_deref(), plugin.version) {
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

/// Materialize the tree, then add our local marketplace if `marketplace list` does
/// not already carry it. copilot has no `marketplace update`; the `current` pointer
/// is stable across versions, so a re-materialize needs no re-add and `plugin
/// update` re-reads the refreshed tree.
fn ensure_marketplace(cli: &CopilotCli, plugin: &Plugin, source: &Source) -> Result<()> {
    let dir = match source {
        Source::Embedded => materialize(plugin, TreeSource::Blob(plugin.blob()))?,
        // A path source materializes its on-disk tree the same way embedded does.
        Source::Path(p) => materialize(plugin, TreeSource::Dir(p))?,
        // GitHub never reaches here (handled by `github_install`).
        Source::GitHub { .. } => return Ok(()),
    };
    if !marketplace_present(cli, plugin.marketplace)? {
        marketplace_add(cli, &dir.display().to_string())?;
    }
    Ok(())
}

/// GitHub source: copilot has no local marketplace to register, so it installs
/// directly. `copilot plugin install owner/repo` is a direct install (deprecated at
/// 1.0.71 but functional); ref-pinning via `owner/repo@ref` is UNVERIFIED for
/// copilot and sent best-effort. copilot assigns a direct install its own
/// marketplace name (not `plugin.marketplace`), so this path is NOT probe-idempotent
/// — an explicit `install` still converges; self_heal cannot recognize it.
fn github_install(cli: &CopilotCli, repo: &str, ref_: &str) -> Result<Outcome> {
    cli.ensure_min_version()?;
    let spec = if ref_.is_empty() { repo.to_string() } else { format!("{repo}@{ref_}") };
    plugin_install(cli, &spec)?;
    Ok(Outcome::Installed)
}

fn verify_present(cli: &CopilotCli, plugin: &Plugin) -> Result<()> {
    if find_plugin(cli, plugin)?.is_some() {
        Ok(())
    } else {
        Err(Error::Verify(format!("{} absent from `copilot plugin list` after the operation", plugin.id())))
    }
}

/// Present-plugin decision, monotonic: update only toward a strictly-newer embedded
/// version. A same-or-newer installed version (e.g. from a coexisting newer binary)
/// is left untouched, so two binaries never downgrade each other.
#[derive(Debug, PartialEq, Eq)]
enum PresentAction {
    NoOp,
    Update,
}

fn present_action(installed: Option<&str>, embedded: &str) -> PresentAction {
    if version_lt(installed, embedded) { PresentAction::Update } else { PresentAction::NoOp }
}

/// Classify presence + version into the self_heal state. copilot's `plugin list`
/// carries no install-path or enabled column, so this is presence + monotonic
/// version only (no `Disabled`, no files-gone `NeedsRepair`).
fn classify(entry: Option<&CopilotPlugin>, embedded: &str) -> BackendState {
    match entry {
        None => BackendState::Absent,
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
