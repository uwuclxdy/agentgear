//! The zed backend — mcp-only. Zed reads MCP servers from a `context_servers`
//! object in its `settings.json` (`~/.config/zed/settings.json`, XDG-based on both
//! Linux and macOS; `%APPDATA%\Zed` on Windows), a flat `{command,args,env}` body
//! identical to the shared json-mcp family's Plain shape. We write only stdio
//! servers, keyed by our plugin's server names, so `remove` is exact and a second
//! reconcile is a true `NoOp`. Zed ships no general hook/command/subagent
//! config-file surface (its `tasks.json` "hooks" fire on a single `create_worktree`
//! event, not a CC lifecycle), so those components are skipped — full mapping and
//! why-skipped detail in `docs/harness/zed.md`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct ZedBackend;

impl AgentBackend for ZedBackend {
    fn id(&self) -> &'static str {
        "zed"
    }

    fn detect(&self) -> bool {
        // The upstream CLI is `zed` (some distros rename it to `zedit`/`zeditor`, but
        // the default name is authoritative); the config dir is XDG-based on both
        // Linux and macOS, so a test redirecting `XDG_CONFIG_HOME`/`HOME` also
        // redirects detection. Zed has no user-config-dir override env of its own.
        which::which("zed").is_ok() || user_config_dir().is_some_and(|d| d.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // mcp only: zed has no config-file hook/command/subagent surface to translate.
        Capabilities { plugins: false, mcp: true, hooks: false, scopes: &["user", "project"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        // Ownership is defined by our mcp server keys (the canonical "are we here"
        // signal); the shared probe returns Healthy — never Absent — for a plugin with
        // no writable servers, so a present marker is never dropped. Source::Embedded is
        // the only steady-state source for a non-CC backend (github unsupported, path is
        // install-only), mirroring the claude probe keying on compile-time metadata.
        let comp = plugin.components(&Source::Embedded)?;
        probe_mcp(&settings_path(scope)?, &comp.mcp_servers)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        reconcile_mcp(&settings_path(scope)?, &comp.mcp_servers)
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        remove_mcp(&settings_path(scope)?, &comp.mcp_servers)
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// Zed's user config dir: `$XDG_CONFIG_HOME/zed` or `~/.config/zed` on Linux AND
/// macOS (zed uses the XDG layout on macOS too, unlike `dirs::config_dir`'s
/// `~/Library/Application Support` platform default — so the non-Windows arm
/// replicates dirs' own XDG-or-home logic to match what zed actually reads);
/// `%APPDATA%\Zed` on Windows.
fn user_config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        dirs::config_dir().map(|c| c.join("Zed"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .map(|c| c.join("zed"))
    }
}

/// The `settings.json` for a scope: the user config dir (a missing config dir is a
/// clear, actionable error, never a silent write to the wrong place) or the
/// worktree-local `<cwd>/.zed/settings.json` a project override lives in.
fn settings_path(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => user_config_dir().map(|d| d.join("settings.json")).ok_or_else(|| {
            Error::Tree("no config directory (XDG_CONFIG_HOME and HOME both unset); cannot locate zed's settings.json".into())
        }),
        Scope::Project { path } => Ok(path.join(".zed").join("settings.json")),
    }
}

// --- mcp ---------------------------------------------------------------------

/// Zed's MCP servers live under this single top-level key.
const CONTEXT_SERVERS: &str = "context_servers";
const MCP_KEY: &[&str] = &[CONTEXT_SERVERS];

/// The servers zed can host: stdio only. Zed's remote-server shape is `{url,headers}`
/// (no `type`/`transport` field), which the shared Plain renderer cannot emit — it
/// would write a stray `type` key — so http/sse servers are skipped rather than
/// written in a shape zed may reject (`docs/harness/zed.md`).
fn stdio_servers(servers: &[McpServer]) -> Vec<McpServer> {
    servers.iter().filter(|s| matches!(s.kind, McpKind::Stdio)).cloned().collect()
}

/// Server keys `reconcile` actually writes (stdio AND portable). `remove` keys off
/// the same set so an unfiltered name can never delete a user server that happens to
/// share a name with one we declared but never wrote (a non-stdio or
/// `${CLAUDE_PLUGIN_ROOT}`-bearing entry).
fn writable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| matches!(s.kind, McpKind::Stdio) && s.is_portable()).map(|s| s.name.as_str()).collect()
}

/// Insert/update our stdio servers under `context_servers`, leaving the user's own.
/// `NoOp` when the file already matches.
fn reconcile_mcp(settings: &Path, servers: &[McpServer]) -> Result<Outcome> {
    mcpjson::reconcile(settings, MCP_KEY, &stdio_servers(servers), ServerShape::Plain)
}

/// Classify `context_servers` for our stdio servers (Absent/Healthy/NeedsRepair).
fn probe_mcp(settings: &Path, servers: &[McpServer]) -> Result<BackendState> {
    mcpjson::probe(settings, MCP_KEY, &stdio_servers(servers), ServerShape::Plain)
}

/// Strip exactly our stdio server keys from `context_servers`, leaving the user's.
fn remove_mcp(settings: &Path, servers: &[McpServer]) -> Result<Outcome> {
    mcpjson::remove(settings, MCP_KEY, &writable_names(servers))
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &ZedBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "zed detected", status: CheckStatus::Ok("`zed` on PATH or a zed config dir present".into()) }
    } else {
        DoctorCheck {
            name: "zed detected",
            status: CheckStatus::Fail {
                problem: "zed not detected".into(),
                fix: "install it with `curl -f https://zed.dev/install.sh | sh`".into(),
            },
        }
    });

    let settings = match settings_path(&Scope::User) {
        Ok(settings) => settings,
        Err(e) => {
            checks.push(DoctorCheck { name: "settings file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };

    let root = match fs::read(&settings) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "settings file", status: CheckStatus::Ok(format!("{} parses", settings.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "settings file",
                    status: CheckStatus::Fail {
                        problem: format!("{} does not parse: {e}", settings.display()),
                        fix: "fix the JSON syntax or remove the file".into(),
                    },
                });
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            checks.push(DoctorCheck {
                name: "settings file",
                status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", settings.display())),
            });
            None
        }
        Err(e) => {
            checks.push(DoctorCheck {
                name: "settings file",
                status: CheckStatus::Warn(format!("could not read {}: {e}", settings.display())),
            });
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
    let expected = writable_names(servers);
    if expected.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no portable mcp servers to register".into()) };
    }
    let obj = root.and_then(|r| r.get(CONTEXT_SERVERS)).and_then(Value::as_object);
    let missing: Vec<&str> = expected.iter().copied().filter(|n| obj.is_none_or(|o| !o.contains_key(*n))).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} registered", expected.join(", "))) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp server(s) not in settings.json: {}", missing.join(", ")),
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
#[path = "../../tests/unit/zed.rs"]
mod zed_tests;
