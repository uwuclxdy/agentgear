//! The kiro (kiro-cli) backend: mcp-only. MCP goes through the shared json
//! renderer (`~/.kiro/settings/mcp.json` `mcpServers`, Plain shape — kiro's local
//! entry is a superset of `{command,args,env}` and defaults the rest), keyed by
//! our server names, so `remove` is exact and a second reconcile is a true `NoOp`.
//!
//! Hooks are declared unsupported (`capabilities().hooks == false`): kiro's only
//! hook surface is a `hooks` object inside a user-owned per-agent config json
//! under `~/.kiro/agents/`, and its run-default agent is a *setting*, not a file —
//! there is no file agentgear can target without editing user-owned agent configs
//! (an earlier version merged into a literal `agents/default.json`, which kiro
//! treats as nothing special, so those hooks never fired). Commands
//! (`~/.kiro/prompts/`) and agents (kiro's own json agent schema) are skipped — see
//! `docs/harness/kiro.md` for why. Skills land as bare
//! `~/.kiro/skills/<name>/SKILL.md` (both scopes), auto-inherited by every kiro agent,
//! tagged for ownership; kiro requires `name`+`description`, which the renderer ensures.

use std::path::PathBuf;

use super::mcpjson::{self, ServerShape};
use super::report;
use super::skillsdir;
use super::{AgentBackend, BackendState};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct KiroBackend;

impl AgentBackend for KiroBackend {
    fn id(&self) -> &'static str {
        "kiro"
    }

    fn detect(&self) -> bool {
        // Binary is `kiro-cli` (bare `kiro` is only an optional Command-Router alias,
        // not the base CLI). The config base is `KIRO_HOME` or `~/.kiro`; a test
        // redirecting either reroutes detection with no `kiro-cli` on PATH.
        which::which("kiro-cli").is_ok() || user_base().is_ok_and(|b| b.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // hooks:false — kiro hosts hooks only inside user-owned agent configs, and
        // its default agent is a setting we cannot reliably target (module doc).
        // mcp + skills translate; commands/agents are skipped.
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: false,
            commands: false,
            agents: false,
            skills: true,
            instructions: false,
            scopes: &["user", "project"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Ownership is our mcp server keys (the "are we here" signal); the shared
        // probe returns Healthy — never Absent — for an mcp-less plugin, so a present
        // marker is never dropped. `source` is the one self_heal resolved for this agent
        // (rehydrated `--path`, else the compile-time default), so probe and reconcile
        // render identical bytes.
        let comp = plugin.components(source)?;
        let mcp = mcpjson::probe(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())?;
        let skills = skillsdir::probe(&kiro_base(scope)?.join("skills"), plugin, &comp.skills)?;
        Ok(report::compose([Some(mcp), skills].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let mut changed = mcpjson::reconcile(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())? != Outcome::NoOp;
        changed |= skillsdir::reconcile(&kiro_base(scope)?.join("skills"), plugin, &comp.skills)?;
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
        let comp = plugin.components(source)?;
        let mut changed = mcpjson::remove(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())? != Outcome::NoOp;
        changed |= skillsdir::remove(&kiro_base(scope)?.join("skills"), plugin, &comp.skills)?;
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// User config base: `KIRO_HOME` (documented override, pointing directly at the
/// `.kiro`-equivalent dir) takes precedence, else `~/.kiro`. Checked first so a
/// test can redirect the whole backend without touching `HOME`.
fn user_base() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("KIRO_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::home_dir().map(|h| h.join(".kiro")).ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.kiro".into()))
}

/// The `.kiro` config base for a scope: user (`KIRO_HOME`/`~/.kiro`) or the
/// project-local `<project>/.kiro`.
fn kiro_base(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => user_base(),
        Scope::Project { path } => Ok(path.join(".kiro")),
    }
}

fn mcp_path(scope: &Scope) -> Result<PathBuf> {
    Ok(kiro_base(scope)?.join("settings").join("mcp.json"))
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &KiroBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "kiro detected", status: CheckStatus::Ok("`kiro-cli` on PATH or ~/.kiro present".into()) }
    } else {
        DoctorCheck {
            name: "kiro detected",
            status: CheckStatus::Fail {
                problem: "kiro CLI not detected".into(),
                fix: "install it with `curl -fsSL https://cli.kiro.dev/install | bash`".into(),
            },
        }
    });

    let mcp = match mcp_path(&Scope::User) {
        Ok(path) => path,
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp config file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };

    let root = report::read_json_config(&mut checks, "mcp config file", &mcp);

    let Some(comp) = report::components(&mut checks, plugin, source) else {
        return checks;
    };

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcpServers"],
        "not in mcp.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));

    checks
}

#[cfg(test)]
#[path = "../../tests/unit/kiro.rs"]
mod kiro_tests;
