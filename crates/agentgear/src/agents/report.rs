//! Shared `doctor` report helpers for the non-CC backends. A backend's `report`
//! assembles a `Vec<DoctorCheck>`; the read-parse boilerplate (open a JSON config,
//! parse it, read the plugin components) and the two mcp checks (server registered,
//! command on PATH) are identical across the config-family backends, so they live
//! here once. A harness with special wording (amp's `settings.jsonc`, openclaw's
//! JSON5, codex/goose's non-JSON configs) keeps its own inline read.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use crate::components::{McpKind, McpServer, PluginComponents};
use crate::doctor::{CheckStatus, DoctorCheck};
use crate::host::{Plugin, Source};

/// Read + parse a JSON config file for a doctor report: push exactly one status
/// check under `name` (parses / does-not-parse / missing / unreadable) and return
/// the parsed root, `None` on any non-Ok outcome.
pub(crate) fn read_json_config(checks: &mut Vec<DoctorCheck>, name: &'static str, path: &Path) -> Option<Value> {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name, status: CheckStatus::Ok(format!("{} parses", path.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name,
                    status: CheckStatus::Fail {
                        problem: format!("{} does not parse: {e}", path.display()),
                        fix: "fix the JSON syntax or remove the file".into(),
                    },
                });
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            checks.push(DoctorCheck { name, status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", path.display())) });
            None
        }
        Err(e) => {
            checks.push(DoctorCheck { name, status: CheckStatus::Warn(format!("could not read {}: {e}", path.display())) });
            None
        }
    }
}

/// Read the plugin's components IR for a doctor report. On error push a Fail check
/// and return `None` so the caller can early-return the report it has so far.
pub(crate) fn components(checks: &mut Vec<DoctorCheck>, plugin: &Plugin, source: &Source) -> Option<PluginComponents> {
    match plugin.components(source) {
        Ok(comp) => Some(comp),
        Err(e) => {
            checks.push(DoctorCheck {
                name: "plugin components",
                status: CheckStatus::Fail {
                    problem: format!("could not read the plugin tree: {e}"),
                    fix: "rebuild the host binary".into(),
                },
            });
            None
        }
    }
}

/// "mcp server registered": every portable server is present under `key_path`.
/// `where_` is the "not <where>" phrase for the failure (e.g. `"not in mcp.json"`,
/// `"not under `mcp` in opencode.json"`) and `fix` its remediation, so each harness
/// keeps its exact wording. Reports `Ok` — never a failure — when the plugin
/// declares no portable server.
pub(crate) fn check_mcp_registered(
    servers: &[McpServer],
    root: Option<&Value>,
    key_path: &[&str],
    where_: &str,
    fix: &str,
) -> DoctorCheck {
    let name = "mcp server registered";
    let portable: Vec<&str> = servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect();
    if portable.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no portable mcp servers to register".into()) };
    }
    let obj = navigate(root, key_path);
    let missing: Vec<&str> = portable.iter().copied().filter(|n| obj.is_none_or(|o| !o.contains_key(*n))).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} registered", portable.join(", "))) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp server(s) {where_}: {}", missing.join(", ")),
                fix: fix.into(),
            },
        }
    }
}

/// "mcp command on PATH": every portable stdio server's command resolves. Only a
/// bare executable name is a PATH lookup; a path/variable command can't be checked
/// generically and is skipped.
pub(crate) fn check_mcp_command(servers: &[McpServer]) -> DoctorCheck {
    let name = "mcp command on PATH";
    let missing: Vec<String> = servers
        .iter()
        .filter(|s| s.is_portable() && matches!(s.kind, McpKind::Stdio))
        .map(|s| s.command.clone())
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

/// Walk `key_path` into `root`, returning the object at the leaf (`None` if any
/// level is absent or not an object). Reproduces a backend's `.get(a).get(b)…` chain.
fn navigate<'a>(root: Option<&'a Value>, key_path: &[&str]) -> Option<&'a Map<String, Value>> {
    let mut cur = root?;
    for key in key_path {
        cur = cur.get(key)?;
    }
    cur.as_object()
}
