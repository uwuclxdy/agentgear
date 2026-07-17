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
//! (`~/.kiro/prompts/`), agents (kiro's own json agent schema) and skills are
//! skipped too — see `docs/harness/kiro.md` for why.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::McpKind;
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
        Capabilities { plugins: false, mcp: true, hooks: false, scopes: &["user", "project"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        // Ownership is our mcp server keys (the "are we here" signal); the shared
        // probe returns Healthy — never Absent — for an mcp-less plugin, so a present
        // marker is never dropped. Source::Embedded is the only steady-state source
        // for a non-CC backend.
        let comp = plugin.components(&Source::Embedded)?;
        let mcp = mcp_path(scope)?;
        mcpjson::probe(&mcp, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        mcpjson::reconcile(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        mcpjson::remove(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())
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

    let root = match fs::read(&mcp) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "mcp config file", status: CheckStatus::Ok(format!("{} parses", mcp.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "mcp config file",
                    status: CheckStatus::Fail {
                        problem: format!("{} does not parse: {e}", mcp.display()),
                        fix: "fix the JSON syntax or remove the file".into(),
                    },
                });
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            checks.push(DoctorCheck {
                name: "mcp config file",
                status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", mcp.display())),
            });
            None
        }
        Err(e) => {
            checks
                .push(DoctorCheck { name: "mcp config file", status: CheckStatus::Warn(format!("could not read {}: {e}", mcp.display())) });
            None
        }
    };

    let comp = match plugin.components(source) {
        Ok(comp) => comp,
        Err(e) => {
            checks.push(DoctorCheck {
                name: "plugin components",
                status: CheckStatus::Fail {
                    problem: format!("could not read the plugin tree: {e}"),
                    fix: "rebuild the host binary".into(),
                },
            });
            return checks;
        }
    };

    checks.push(check_mcp_registered(&comp.mcp_servers, root.as_ref()));
    checks.push(check_mcp_command(&comp.mcp_servers));

    checks
}

fn check_mcp_registered(servers: &[crate::components::McpServer], root: Option<&Value>) -> DoctorCheck {
    let name = "mcp server registered";
    let portable: Vec<&str> = servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect();
    if portable.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no portable mcp servers to register".into()) };
    }
    let obj = root.and_then(|r| r.get("mcpServers")).and_then(Value::as_object);
    let missing: Vec<&str> = portable.iter().copied().filter(|n| obj.is_none_or(|o| !o.contains_key(*n))).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} registered", portable.join(", "))) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp server(s) not in mcp.json: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

fn check_mcp_command(servers: &[crate::components::McpServer]) -> DoctorCheck {
    let name = "mcp command on PATH";
    let missing: Vec<String> = servers
        .iter()
        .filter(|s| s.is_portable() && matches!(s.kind, McpKind::Stdio))
        .map(|s| s.command.clone())
        // Only a bare executable name is a PATH lookup; a path/variable command can't be checked generically.
        .filter(|c| !c.is_empty() && !c.contains('/') && !c.contains('\\') && !c.contains('$') && which::which(c).is_err())
        .collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok("all referenced mcp commands resolve".into()) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp command(s) not on PATH: {}", missing.join(", ")),
                fix: "install the missing binaries into a PATH directory".into(),
            },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/kiro.rs"]
mod kiro_tests;
