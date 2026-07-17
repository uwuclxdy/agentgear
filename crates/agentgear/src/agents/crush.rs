//! The crush (charmbracelet) backend: a full translate into crush's own config,
//! `~/.config/crush/crush.json` (override `CRUSH_GLOBAL_CONFIG`). MCP and hooks
//! share that one file — the root `mcp` map and the root `hooks` key — so a
//! reconcile is a single `json_edit` covering both. MCP reuses the shared json
//! renderer's `Typed` shape (`{type:"stdio",command,args,env}`; crush requires an
//! explicit per-server `type`, enum `stdio|http|sse`); hooks are flat
//! `{command,matcher?}` entries under `hooks.PreToolUse`. Every mcp key is our own
//! server name and every hook is matched by its command string, so `remove` is
//! exact and a second reconcile is a true `NoOp`.
//!
//! Skipped surfaces (see `docs/harness/crush.md`):
//! - **commands** and **agents**: crush has no file-writable surface for either yet
//!   (issues #2219 / #1807 open), so `commands`/`agents` are dropped, not guessed.
//! - **skills**: out of scope for v1 (crush does directory-load skills, a real
//!   future surface).
//! - **hook events other than `PreToolUse`**: crush defines only that one event
//!   today; every other CC event has no analog and is skipped.
//!
//! Unlike codex, crush hooks are NOT trust-gated — a written hook fires
//! automatically, so a translated hook is live the moment crush next reads the file.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::cchooks::hook_is_portable;
use super::confedit::{json_edit, json_obj_at};
use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct CrushBackend;

impl AgentBackend for CrushBackend {
    fn id(&self) -> &'static str {
        "crush"
    }

    fn detect(&self) -> bool {
        // `~/.config/crush` is XDG-based with a documented `CRUSH_GLOBAL_CONFIG`
        // override, so a test redirecting either points detection at the same temp
        // dir; the `crush` CLI on PATH is a bonus, its absence never implies absent.
        which::which("crush").is_ok() || crush_config_dir_opt().is_some_and(|d| d.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { plugins: false, mcp: true, hooks: true, scopes: &["user", "project"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        // Ownership is defined by our mcp server keys (the canonical "are we here"
        // signal); the shared probe returns Healthy — never Absent — for an mcp-less
        // plugin, so a present marker is never dropped. Source::Embedded is the only
        // steady-state source for a non-CC backend (github unsupported, path is
        // install-only), mirroring the gemini/claude probe keying on compile-time metadata.
        let comp = plugin.components(&Source::Embedded)?;
        mcpjson::probe(&config_file(scope)?, &["mcp"], &comp.mcp_servers, ServerShape::typed())
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let changed = reconcile_config(&config_file(scope)?, &comp.mcp_servers, &comp.hooks)?;
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let changed = remove_config(&config_file(scope)?, &portable_names(&comp.mcp_servers), &comp.hooks)?;
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The global crush config dir, honoring `CRUSH_GLOBAL_CONFIG` (its documented
/// override) then `$XDG_CONFIG_HOME/crush`. `_opt` never errors so `detect` can
/// call it; the env/XDG fallbacks mean a test redirecting either redirects both
/// detection and the write target.
fn crush_config_dir_opt() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CRUSH_GLOBAL_CONFIG").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    // Local (not Roaming) AppData on Windows — crush reads `%LOCALAPPDATA%\crush`;
    // on Linux/macOS this is identical to `config_dir` (`$XDG_CONFIG_HOME`).
    dirs::config_local_dir().map(|c| c.join("crush"))
}

fn crush_config_dir() -> Result<PathBuf> {
    crush_config_dir_opt().ok_or_else(|| {
        Error::Tree("no config directory (HOME/XDG_CONFIG_HOME/CRUSH_GLOBAL_CONFIG unset); cannot locate ~/.config/crush".into())
    })
}

/// The single `crush.json` we read-modify-write for a scope: the global
/// `~/.config/crush/crush.json` (user) or `<cwd>/crush.json` (project — crush lets
/// a project config override the global, per the brief).
fn config_file(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(crush_config_dir()?.join("crush.json")),
        Scope::Project { path } => Ok(path.join("crush.json")),
    }
}

/// Server names `reconcile` actually writes (non-portable ones are skipped).
/// `remove` keys off the same set so it never deletes a user server that happens
/// to share a name with one we declared but never wrote.
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
}

// --- hooks -------------------------------------------------------------------

/// Crush defines exactly one hook event today — `PreToolUse` — matched
/// case-insensitively but written in its canonical casing. Every other CC event
/// has no crush analog and is skipped rather than written under a guessed name.
fn map_event(cc_event: &str) -> Option<&'static str> {
    cc_event.eq_ignore_ascii_case("PreToolUse").then_some("PreToolUse")
}

/// A crush hook entry is a flat `{command, matcher?}` object directly in the event
/// array (no CC-style nested `hooks` list); the optional `name`/`timeout` fields are
/// omitted (crush defaults `timeout` to 30 and `name` is a cosmetic TUI label).
fn render_hook_entry(hook: &HookBinding) -> Value {
    let mut obj = Map::new();
    obj.insert("command".into(), Value::from(hook.command.clone()));
    if let Some(matcher) = &hook.matcher {
        obj.insert("matcher".into(), Value::from(matcher.clone()));
    }
    Value::Object(obj)
}

// --- reconcile / remove (one file, one edit) ---------------------------------

/// Insert/update our mcp servers under the root `mcp` map and add-if-absent our
/// `PreToolUse` hook entries under `hooks.PreToolUse`, in a single `json_edit` (mcp
/// and hooks live in the same `crush.json`). Non-portable servers/hooks and events
/// with no crush analog are skipped; when nothing survives, `json_edit` is not
/// entered so no empty `mcp`/`hooks` key is created for a plugin with nothing to
/// translate. Idempotent: a hook already present (deep-equal) is not re-added, and
/// an unchanged document skips the write -> a true `NoOp`.
fn reconcile_config(config: &Path, servers: &[McpServer], hooks: &[HookBinding]) -> Result<bool> {
    let portable: Vec<&McpServer> = servers.iter().filter(|s| s.is_portable()).collect();
    let writable_hooks: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|e| (e, h))).collect();
    if portable.is_empty() && writable_hooks.is_empty() {
        return Ok(false);
    }
    json_edit(config, |root| {
        if !portable.is_empty() {
            let mcp = json_obj_at(root, &["mcp"]);
            for server in &portable {
                if let Some(body) = mcpjson::render_server(server, ServerShape::typed()) {
                    mcp.insert(server.name.clone(), body);
                }
            }
        }
        if !writable_hooks.is_empty() {
            let events = json_obj_at(root, &["hooks"]);
            for (event, hook) in &writable_hooks {
                let entry = render_hook_entry(hook);
                let list = events.entry((*event).to_string()).or_insert_with(|| Value::Array(Vec::new()));
                if let Value::Array(arr) = list
                    && !arr.iter().any(|e| e == &entry)
                {
                    arr.push(entry);
                }
            }
        }
        Ok(())
    })
}

/// Strip exactly our mcp server keys and our hook entries (matched by command
/// string) from the one `crush.json`, dropping a hook event array we emptied. A
/// user server or hook sharing a key/event with ours survives; the file itself is
/// left in place (merge-safe).
fn remove_config(config: &Path, server_names: &[&str], hooks: &[HookBinding]) -> Result<bool> {
    if !config.exists() {
        return Ok(false);
    }
    // Match reconcile's writable-hook filter exactly (portable AND a mapped crush
    // event): a portable hook under an unmapped CC event was never written, so its
    // command must never be a removal candidate — and we only ever touch the crush
    // events we manage, never a user's own event array.
    let ours: BTreeSet<&str> =
        hooks.iter().filter(|h| hook_is_portable(h) && map_event(&h.event).is_some()).map(|h| h.command.as_str()).collect();
    let managed: BTreeSet<&str> = hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event)).collect();
    json_edit(config, |root| {
        if let Some(mcp) = root.get_mut("mcp").and_then(Value::as_object_mut) {
            for name in server_names {
                mcp.remove(*name);
            }
        }
        if let Some(events) = root.get_mut("hooks").and_then(Value::as_object_mut) {
            // Only clean up an event array we actually emptied — a user's pre-existing
            // empty array under an event we manage (or any unmanaged event) survives.
            let mut emptied: Vec<String> = Vec::new();
            for event in &managed {
                if let Some(arr) = events.get_mut(*event).and_then(Value::as_array_mut) {
                    let before = arr.len();
                    arr.retain(|e| e.get("command").and_then(Value::as_str).is_none_or(|c| !ours.contains(c)));
                    if arr.len() < before && arr.is_empty() {
                        emptied.push((*event).to_string());
                    }
                }
            }
            for event in emptied {
                events.remove(&event);
            }
        }
        Ok(())
    })
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &CrushBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "crush detected", status: CheckStatus::Ok("`crush` on PATH or ~/.config/crush present".into()) }
    } else {
        DoctorCheck {
            name: "crush detected",
            status: CheckStatus::Fail {
                problem: "crush CLI not detected".into(),
                fix: "install it with `npm install -g @charmland/crush`".into(),
            },
        }
    });

    let config = match config_file(&Scope::User) {
        Ok(config) => config,
        Err(e) => {
            checks.push(DoctorCheck { name: "config file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };

    let root = match fs::read(&config) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "config file", status: CheckStatus::Ok(format!("{} parses", config.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "config file",
                    status: CheckStatus::Fail {
                        problem: format!("{} does not parse: {e}", config.display()),
                        fix: "fix the JSON syntax or remove the file".into(),
                    },
                });
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            checks.push(DoctorCheck {
                name: "config file",
                status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", config.display())),
            });
            None
        }
        Err(e) => {
            checks
                .push(DoctorCheck { name: "config file", status: CheckStatus::Warn(format!("could not read {}: {e}", config.display())) });
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
    checks.push(check_hooks_present(&comp.hooks, root.as_ref()));

    checks
}

fn check_mcp_registered(servers: &[McpServer], root: Option<&Value>) -> DoctorCheck {
    let name = "mcp server registered";
    let portable: Vec<&str> = servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect();
    if portable.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no portable mcp servers to register".into()) };
    }
    let obj = root.and_then(|r| r.get("mcp")).and_then(Value::as_object);
    let missing: Vec<&str> = portable.iter().copied().filter(|n| obj.is_none_or(|o| !o.contains_key(*n))).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} registered", portable.join(", "))) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp server(s) not under `mcp` in crush.json: {}", missing.join(", ")),
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

/// Only `PreToolUse` hooks translate; a plugin whose hooks are all other events (or
/// all non-portable) has nothing to check. A present hook is a plain `Ok` — crush
/// hooks are not trust-gated, so they fire the moment crush reads the file.
fn check_hooks_present(hooks: &[HookBinding], root: Option<&Value>) -> DoctorCheck {
    let name = "translated hooks present";
    let ours: Vec<&str> =
        hooks.iter().filter(|h| hook_is_portable(h) && map_event(&h.event).is_some()).map(|h| h.command.as_str()).collect();
    if ours.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no PreToolUse hooks to translate".into()) };
    }
    let commands: BTreeSet<&str> = root
        .and_then(|r| r.get("hooks"))
        .and_then(|h| h.get("PreToolUse"))
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|e| e.get("command").and_then(Value::as_str)).collect())
        .unwrap_or_default();
    let missing: Vec<&str> = ours.iter().copied().filter(|c| !commands.contains(c)).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} PreToolUse hook(s) present", ours.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("PreToolUse hook(s) missing from crush.json: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/crush.rs"]
mod crush_tests;
