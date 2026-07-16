//! The kiro (kiro-cli) backend: a config-merge translate of the CC plugin's mcp
//! servers + hooks into kiro's own surfaces. MCP goes through the shared json
//! renderer (`~/.kiro/settings/mcp.json` `mcpServers`, Plain shape — kiro's local
//! entry is a superset of `{command,args,env}` and defaults the rest). Hooks are
//! special: kiro-cli has no standalone hooks file — they live in a `hooks` object
//! **inside an agent's own `.json`** under `~/.kiro/agents/`. We merge only into
//! the default agent (`agents/default.json`) and only if that file already exists,
//! so we never fabricate an agent definition (a hooks-only file is not a valid
//! agent). Everything we write is keyed by our server names / hook command strings,
//! so `remove` is exact and a second reconcile is a true `NoOp`.
//!
//! Commands (`~/.kiro/prompts/`), agents (kiro's own json agent schema) and skills
//! are skipped — see `docs/harness/kiro.md` for why.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::confedit::{json_edit, json_obj_at};
use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, McpKind};
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
        Capabilities { plugins: false, mcp: true, hooks: true, scopes: &["user", "project"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        // Ownership is our mcp server keys (the "are we here" signal); the shared
        // probe returns Healthy — never Absent — for an mcp-less plugin, so a present
        // marker is never dropped. Hooks are best-effort (they only land when the
        // default agent file exists), so they do not define install state.
        // Source::Embedded is the only steady-state source for a non-CC backend.
        let comp = plugin.components(&Source::Embedded)?;
        let mcp = mcp_path(scope)?;
        mcpjson::probe(&mcp, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;

        let mut changed = false;
        changed |= mcpjson::reconcile(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())? != Outcome::NoOp;
        changed |= reconcile_hooks(&default_agent_path(scope)?, &comp.hooks)?;
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;

        let mut changed = false;
        changed |= mcpjson::remove(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())? != Outcome::NoOp;
        changed |= remove_hooks(&default_agent_path(scope)?, &comp.hooks)?;
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

/// The default agent config file we merge hooks into. Kiro has no standalone hooks
/// file; hooks live inside an agent's `.json`, and `default` is its default agent.
/// We only ever merge into this file when it already exists (never create it).
fn default_agent_path(scope: &Scope) -> Result<PathBuf> {
    Ok(kiro_base(scope)?.join("agents").join("default.json"))
}

// --- hooks -------------------------------------------------------------------

/// Map a CC hook event to kiro-cli's camelCase analog. `SessionStart`->`agentSpawn`
/// (fires when an agent spins up), `UserPromptSubmit`->`userPromptSubmit`,
/// `PreToolUse`/`PostToolUse`/`Stop` map by name. Events with no kiro analog
/// (`SessionEnd`, `SubagentStop`, `PreCompact`, `Notification`) are skipped rather
/// than written under a guessed name.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "SessionStart" => Some("agentSpawn"),
        "UserPromptSubmit" => Some("userPromptSubmit"),
        "PreToolUse" => Some("preToolUse"),
        "PostToolUse" => Some("postToolUse"),
        "Stop" => Some("stop"),
        _ => None,
    }
}

/// Kiro documents `matcher` only for `preToolUse`/`postToolUse`; for the lifecycle
/// events a matcher is meaningless, so it is dropped even if the CC hook carried one.
fn event_supports_matcher(kiro_event: &str) -> bool {
    matches!(kiro_event, "preToolUse" | "postToolUse")
}

/// A `${CLAUDE_PLUGIN_ROOT}` reference only expands inside Claude Code's own hook
/// runner (kiro has no equivalent token — its `${VAR}` expansion is OS-env only),
/// so such a command would spawn as the literal, unexpanded string. Mirrors
/// `McpServer::is_portable`; applied locally since `HookBinding` has no such method.
fn hook_is_portable(hook: &HookBinding) -> bool {
    !hook.command.contains("${CLAUDE_PLUGIN_ROOT}")
}

/// Kiro's per-hook entry: `{command, matcher?}`. `timeout_ms`/`cache_ttl_seconds`
/// are left to kiro's defaults (minimal, so a re-reconcile stays byte-identical).
fn render_hook_entry(kiro_event: &str, hook: &HookBinding) -> Value {
    let mut obj = Map::new();
    obj.insert("command".into(), Value::from(hook.command.clone()));
    if event_supports_matcher(kiro_event)
        && let Some(matcher) = &hook.matcher
    {
        obj.insert("matcher".into(), Value::from(matcher.clone()));
    }
    Value::Object(obj)
}

/// Add-if-absent our hook entries under each mapped event inside the agent file's
/// `hooks` object, leaving the user's own entries. Idempotent (an entry already
/// present deep-equal is not re-added). **Never fabricates**: if the agent file does
/// not exist, hooks are skipped entirely rather than writing a hooks-only file that
/// is not a valid agent definition. Non-portable hooks and unmapped events are
/// skipped, same as mcp servers.
fn reconcile_hooks(agent_file: &Path, hooks: &[HookBinding]) -> Result<bool> {
    let writable: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|event| (event, h))).collect();
    // Skip before any file check when there is nothing to write, and never create
    // the agent file: hooks only attach to an agent the user already owns.
    if writable.is_empty() || !agent_file.exists() {
        return Ok(false);
    }
    json_edit(agent_file, |root| {
        let events = json_obj_at(root, &["hooks"]);
        for (event, hook) in &writable {
            let entry = render_hook_entry(event, hook);
            let list = events.entry((*event).to_string()).or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(list) = list
                && !list.iter().any(|e| e == &entry)
            {
                list.push(entry);
            }
        }
        Ok(())
    })
}

/// Strip exactly our hook entries (matched by command string) from every event in
/// the agent file's `hooks` object, dropping an event array we emptied and the whole
/// `hooks` object if nothing of ours or the user's remains. A user entry (under any
/// event) survives.
fn remove_hooks(agent_file: &Path, hooks: &[HookBinding]) -> Result<bool> {
    if !agent_file.exists() {
        return Ok(false);
    }
    let ours: BTreeSet<&str> = hooks.iter().filter(|h| hook_is_portable(h)).map(|h| h.command.as_str()).collect();
    json_edit(agent_file, |root| {
        let Some(map) = root.as_object_mut() else { return Ok(()) };
        let emptied = {
            let Some(events) = map.get_mut("hooks").and_then(Value::as_object_mut) else {
                return Ok(());
            };
            for entries in events.values_mut() {
                if let Some(list) = entries.as_array_mut() {
                    list.retain(|e| e.get("command").and_then(Value::as_str).is_none_or(|c| !ours.contains(c)));
                }
            }
            events.retain(|_, entries| entries.as_array().is_none_or(|a| !a.is_empty()));
            events.is_empty()
        };
        // Drop a `hooks` key we emptied so an uninstall leaves no residue in the
        // user's agent file. A user's own hook keeps `events` non-empty -> kept.
        if emptied {
            map.remove("hooks");
        }
        Ok(())
    })
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
