//! The antigravity-cli (`agy`) backend: a translate into Antigravity's own file
//! config. MCP goes through the shared json renderer under the SHARED
//! `~/.gemini/config/mcp_config.json` `mcpServers` key, `ServerShape::plain()`
//! (`{command,args,env}`) — byte-identical to the antigravity desktop backend
//! (same file, same key, same renderer), so a double-install across the two
//! antigravity backends is a true `NoOp`. Hooks land in the CLI's own
//! `~/.gemini/antigravity-cli/hooks.json`, keyed by our plugin name at the top
//! level (`{"<plugin>":{"<Event>":[...]}}`), so we own that whole subtree and
//! `remove` is an exact single-key delete. Commands/agents/skills/rules are
//! skipped — MCP is the only surface backed by an official-Google source, so the
//! rest is left out per the research brief (see `docs/harness/antigravity-cli.md`).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::confedit::json_edit;
use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct AntigravityCliBackend;

impl AgentBackend for AntigravityCliBackend {
    fn id(&self) -> &'static str {
        "antigravity-cli"
    }

    fn detect(&self) -> bool {
        // `agy` on PATH is the direct signal; `~/.gemini/antigravity-cli/` is the
        // CLI-specific config dir (distinct from the shared `~/.gemini/config/`),
        // so a test redirecting `HOME` redirects detection. `ANTIGRAVITY_API_KEY`
        // is an auth INPUT a user may export without the CLI installed, so it is
        // deliberately NOT a detection signal.
        which::which("agy").is_ok() || dirs::home_dir().is_some_and(|h| h.join(".gemini").join("antigravity-cli").is_dir())
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
        let mcp = mcp_path(scope)?;
        mcpjson::probe(&mcp, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;

        let mut changed = false;
        changed |= mcpjson::reconcile(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())? != Outcome::NoOp;
        changed |= reconcile_hooks(&hooks_path(scope)?, plugin.name, &comp.hooks)?;
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;

        let mut changed = false;
        changed |= mcpjson::remove(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())? != Outcome::NoOp;
        changed |= remove_hooks(&hooks_path(scope)?, plugin.name)?;
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// `~/.gemini` — the tree Antigravity 2.0 (desktop IDE + CLI) share. User scope
/// needs `HOME`; a missing home is a clear, actionable error.
fn gemini_home() -> Result<PathBuf> {
    dirs::home_dir().map(|h| h.join(".gemini")).ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.gemini".into()))
}

/// The MCP config: user scope = the SHARED `~/.gemini/config/mcp_config.json`
/// (read by both the desktop IDE and `agy`); project scope = Antigravity's native
/// per-workspace `<root>/.agents/mcp_config.json` (best-effort — the CLI supports
/// it but IDE parity is contested, see the brief).
fn mcp_path(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(gemini_home()?.join("config").join("mcp_config.json")),
        Scope::Project { path } => Ok(path.join(".agents").join("mcp_config.json")),
    }
}

/// The hooks config: user scope = the CLI's own `~/.gemini/antigravity-cli/hooks.json`
/// (NOT the shared `config/` dir — global hooks live under the CLI-specific dir);
/// project scope = `<root>/.agents/hooks.json` (loads only after the folder is
/// trusted).
fn hooks_path(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(gemini_home()?.join("antigravity-cli").join("hooks.json")),
        Scope::Project { path } => Ok(path.join(".agents").join("hooks.json")),
    }
}

// --- hooks -------------------------------------------------------------------

/// Map a CC hook event to an Antigravity hook event, using only names the brief
/// enumerates. `SessionStart`/`PreToolUse`/`PostToolUse`/`Stop` are identity
/// matches (Antigravity carries the literal CC names); `UserPromptSubmit` ->
/// `BeforeAgent` is the closest analog (fires before the agent loop, same as the
/// gemini mapping). Events with no listed counterpart (`SessionEnd`,
/// `SubagentStop`, `PreCompact`, `Notification`) are skipped, never guessed.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "SessionStart" => Some("SessionStart"),
        "UserPromptSubmit" => Some("BeforeAgent"),
        "PreToolUse" => Some("PreToolUse"),
        "PostToolUse" => Some("PostToolUse"),
        "Stop" => Some("Stop"),
        _ => None,
    }
}

/// A `${CLAUDE_PLUGIN_ROOT}` reference only expands inside Claude Code's own hook
/// runner; Antigravity has no equivalent substitution (it requires absolute paths,
/// no `${workspaceFolder}` token), so such a command would spawn as the literal,
/// unexpanded token. Mirrors `McpServer::is_portable` (applied locally since
/// `HookBinding` has no such method in the shared IR).
fn hook_is_portable(hook: &HookBinding) -> bool {
    !hook.command.contains("${CLAUDE_PLUGIN_ROOT}")
}

/// One Antigravity hook entry: flat `{matcher?, type:"command", command}` — the
/// shape `agy` reads, with matcher/type/command as siblings (Antigravity ties one
/// matcher to one command), NOT the CC-nested `{matcher?, hooks:[{type, command}]}`
/// group. `timeout` is omitted: the components IR carries none and Antigravity
/// supplies its own default.
fn render_hook_entry(hook: &HookBinding) -> Value {
    let mut entry = Map::new();
    if let Some(matcher) = &hook.matcher {
        entry.insert("matcher".into(), Value::from(matcher.clone()));
    }
    entry.insert("type".into(), Value::from("command"));
    entry.insert("command".into(), Value::from(hook.command.clone()));
    Value::Object(entry)
}

/// Write our whole hook subtree under the top-level `<plugin>` key
/// (`{"<plugin>":{"<Event>":[entry,...]}}`). We own that key, so a wholesale set
/// is exact and idempotent: `json_edit` skips the write when the rebuilt subtree
/// deep-equals the existing one. Non-portable hooks and events with no Antigravity
/// analog are dropped before we touch the file; when nothing survives, `json_edit`
/// is not entered so no empty `hooks.json` is created.
fn reconcile_hooks(path: &Path, plugin: &str, hooks: &[HookBinding]) -> Result<bool> {
    let writable: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|event| (event, h))).collect();
    if writable.is_empty() {
        return Ok(false);
    }
    json_edit(path, |root| {
        let mut events: Map<String, Value> = Map::new();
        for (event, hook) in &writable {
            let entry = render_hook_entry(hook);
            if let Value::Array(list) = events.entry((*event).to_string()).or_insert_with(|| Value::Array(Vec::new())) {
                list.push(entry);
            }
        }
        if let Value::Object(map) = root {
            map.insert(plugin.to_string(), Value::Object(events));
        }
        Ok(())
    })
}

/// Drop exactly our top-level `<plugin>` key, leaving every other plugin's (or the
/// user's own) top-level hook entry untouched. Since we only ever wrote portable
/// hooks under our own key, a whole-key delete can never reach a foreign entry —
/// no per-command portability filter is needed here.
fn remove_hooks(path: &Path, plugin: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    json_edit(path, |root| {
        if let Value::Object(map) = root {
            map.remove(plugin);
        }
        Ok(())
    })
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &AntigravityCliBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck {
            name: "antigravity-cli detected",
            status: CheckStatus::Ok("`agy` on PATH or ~/.gemini/antigravity-cli present".into()),
        }
    } else {
        DoctorCheck {
            name: "antigravity-cli detected",
            status: CheckStatus::Fail {
                problem: "antigravity-cli (`agy`) not detected".into(),
                fix: "install it with `curl -fsSL https://antigravity.google/cli/install.sh | bash`".into(),
            },
        }
    });

    let mcp = match mcp_path(&Scope::User) {
        Ok(mcp) => mcp,
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp_config.json", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };

    let root = match fs::read(&mcp) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "mcp_config.json", status: CheckStatus::Ok(format!("{} parses", mcp.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "mcp_config.json",
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
                name: "mcp_config.json",
                status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", mcp.display())),
            });
            None
        }
        Err(e) => {
            checks
                .push(DoctorCheck { name: "mcp_config.json", status: CheckStatus::Warn(format!("could not read {}: {e}", mcp.display())) });
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
    checks.push(check_hooks_registered(plugin.name, &comp.hooks));

    checks
}

fn check_mcp_registered(servers: &[McpServer], root: Option<&Value>) -> DoctorCheck {
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
                problem: format!("mcp server(s) not in mcp_config.json: {}", missing.join(", ")),
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

fn check_hooks_registered(plugin: &str, hooks: &[HookBinding]) -> DoctorCheck {
    let name = "hooks registered";
    let writable = hooks.iter().filter(|h| hook_is_portable(h)).any(|h| map_event(&h.event).is_some());
    if !writable {
        return DoctorCheck { name, status: CheckStatus::Ok("no portable, mappable hooks to register".into()) };
    }
    let path = match hooks_path(&Scope::User) {
        Ok(p) => p,
        Err(e) => return DoctorCheck { name, status: CheckStatus::Warn(e.to_string()) },
    };
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) if v.get(plugin).is_some() => DoctorCheck { name, status: CheckStatus::Ok(format!("{plugin} hook entry present")) },
            Ok(_) => DoctorCheck {
                name,
                status: CheckStatus::Fail {
                    problem: format!("no `{plugin}` entry in {}", path.display()),
                    fix: "run the host's `setup`".into(),
                },
            },
            Err(e) => DoctorCheck {
                name,
                status: CheckStatus::Fail {
                    problem: format!("{} does not parse: {e}", path.display()),
                    fix: "fix the JSON syntax or remove the file".into(),
                },
            },
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            DoctorCheck { name, status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", path.display())) }
        }
        Err(e) => DoctorCheck { name, status: CheckStatus::Warn(format!("could not read {}: {e}", path.display())) },
    }
}

#[cfg(test)]
#[path = "../../tests/unit/antigravity_cli.rs"]
mod antigravity_cli_tests;
