//! The JetBrains-Copilot backend: GitHub Copilot's JetBrains IDE plugin. MCP-only
//! in practice — the plugin has no CLI and its only user-writable, file-backed
//! surface is `<config>/github-copilot/intellij/mcp.json` (root key `servers`,
//! stdio entries shaped `{type:"stdio",command,args,env}`). Written through the
//! shared json renderer (`ServerShape::Typed`), so `remove` is exact (only our
//! server keys) and a second reconcile is a true `NoOp`. Hooks/commands/agents are
//! repo-level `.github` surfaces (Copilot-CLI / vscode-copilot territory) with no
//! JetBrains-plugin file consumer, so they are skipped — see
//! `docs/harness/jetbrains-copilot.md`.
//!
//! User-scope only: capabilities advertise just `user` and the orchestration skips
//! any other scope, so the lifecycle methods ignore their `scope` argument.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct JetbrainsCopilotBackend;

/// mcp.json nests servers under a bare `servers` object (not VS Code's / CC's
/// `mcpServers`); stdio entries carry an explicit `"type":"stdio"`.
const MCP_KEY: &[&str] = &["servers"];
const SHAPE: ServerShape = ServerShape::Typed;

impl AgentBackend for JetbrainsCopilotBackend {
    fn id(&self) -> &'static str {
        "jetbrains-copilot"
    }

    fn detect(&self) -> bool {
        // No CLI on PATH (it's an IDE plugin bundling copilot-language-server), so the
        // JetBrains-specific `github-copilot/intellij` config dir is the only signal.
        // Keyed on that subdir, not the shared `github-copilot` parent, so we don't fire
        // when only the VS Code Copilot backend's config is present. On Unix `config_base()`
        // is `$HOME`-based, so a HOME-redirecting test also redirects detection; on Windows
        // it resolves LocalAppData via SHGetKnownFolderPath (env-independent), so the
        // hermetic test stays Unix-only.
        config_base().is_some_and(|b| b.join("github-copilot").join("intellij").is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { plugins: false, mcp: true, hooks: false, scopes: &["user"] }
    }

    fn probe(&self, plugin: &Plugin, _scope: &Scope) -> Result<BackendState> {
        // Ownership is our mcp server keys. Source::Embedded is the only steady-state
        // source for a non-CC backend (github unsupported, path install-only); the shared
        // probe returns Healthy — never Absent — for a plugin with no portable servers, so
        // a present marker is never dropped.
        let comp = plugin.components(&Source::Embedded)?;
        mcpjson::probe(&mcp_path()?, MCP_KEY, &comp.mcp_servers, SHAPE)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, _scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        mcpjson::reconcile(&mcp_path()?, MCP_KEY, &comp.mcp_servers, SHAPE)
    }

    fn remove(&self, plugin: &Plugin, _scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        // Key removal off the same portable-server set reconcile writes: an unfiltered name
        // could delete an unrelated user server sharing a name with a non-portable entry we
        // never wrote (e.g. a `${CLAUDE_PLUGIN_ROOT}`-bearing one).
        mcpjson::remove(&mcp_path()?, MCP_KEY, &portable_names(&comp.mcp_servers))
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// GitHub Copilot's config root: hardcoded to `~/.config/github-copilot` on every
/// Unix (macOS included — NOT `~/Library`) and `%LOCALAPPDATA%\github-copilot` on
/// Windows. Built from `dirs::home_dir()` on Unix rather than `dirs::config_dir()`
/// (which resolves to `~/Library/Application Support` on macOS and would miss the
/// real path), so a test redirecting `$HOME` also redirects us.
#[cfg(not(windows))]
fn config_base() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config"))
}

#[cfg(windows)]
fn config_base() -> Option<PathBuf> {
    dirs::data_local_dir()
}

/// `<config>/github-copilot/intellij/mcp.json` — the JetBrains plugin's only
/// user-scope MCP file (no project-level file exists).
fn mcp_path() -> Result<PathBuf> {
    let base = config_base()
        .ok_or_else(|| Error::Tree("no config directory (HOME/LOCALAPPDATA unset); cannot locate github-copilot/intellij".into()))?;
    Ok(base.join("github-copilot").join("intellij").join("mcp.json"))
}

/// Server names `reconcile` actually writes (the shared renderer skips non-portable
/// ones); `remove` must key off the same set.
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &JetbrainsCopilotBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "jetbrains-copilot detected", status: CheckStatus::Ok("`github-copilot/intellij` config dir present".into()) }
    } else {
        DoctorCheck {
            name: "jetbrains-copilot detected",
            status: CheckStatus::Warn("no `github-copilot/intellij` config dir; the JetBrains Copilot plugin isn't set up here".into()),
        }
    });

    let path = match mcp_path() {
        Ok(path) => path,
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp.json", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };

    let root = match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "mcp.json", status: CheckStatus::Ok(format!("{} parses", path.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "mcp.json",
                    status: CheckStatus::Fail {
                        problem: format!("{} does not parse: {e}", path.display()),
                        fix: "fix the JSON syntax or remove the file".into(),
                    },
                });
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            checks.push(DoctorCheck {
                name: "mcp.json",
                status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", path.display())),
            });
            None
        }
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp.json", status: CheckStatus::Warn(format!("could not read {}: {e}", path.display())) });
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

fn check_mcp_registered(servers: &[McpServer], root: Option<&Value>) -> DoctorCheck {
    let name = "mcp server registered";
    let portable: Vec<&str> = portable_names(servers);
    if portable.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no portable mcp servers to register".into()) };
    }
    let obj = root.and_then(|r| r.get("servers")).and_then(Value::as_object);
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

fn check_mcp_command(servers: &[McpServer]) -> DoctorCheck {
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
#[path = "../../tests/unit/jetbrains_copilot.rs"]
mod jetbrains_copilot_tests;
