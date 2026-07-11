//! The qwen-code backend: a full translate into qwen-code's own config. qwen-code
//! is a Claude-Code-shaped fork, so translation is mostly a re-emit. MCP goes
//! through the shared json renderer (Plain `{command,args,env}`) into the
//! `mcpServers` key of `~/.qwen/settings.json`; CC hooks land in that same file
//! under a `hooks` key using CC's identical nested shape (qwen-code's event names —
//! `SessionStart`/`UserPromptSubmit`/`PreToolUse`/… — match CC 1:1); CC commands
//! copy through verbatim as markdown under `~/.qwen/commands/<plugin>/` (subdirs
//! preserved for qwen's `:` namespacing); CC agents become plugin-prefixed
//! `~/.qwen/agents/<plugin>-<name>.md` subagent files.
//!
//! Ownership: mcp servers are keyed by our server names; commands live in a
//! `commands/<plugin>/` subtree we own whole; agent files are plugin-prefixed. So
//! `remove` is exact and a second reconcile is a true `NoOp`. `~/.qwen` is honored
//! via `QWEN_HOME` first (so a test redirecting it redirects the backend), else
//! HOME-based. Skills have no qwen file surface and are skipped (see
//! `docs/harness/qwen-code.md`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::confedit::{json_edit, json_obj_at, remove_file_idem, write_file_idem};
use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, IoContext, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct QwenCodeBackend;

impl AgentBackend for QwenCodeBackend {
    fn id(&self) -> &'static str {
        "qwen-code"
    }

    fn detect(&self) -> bool {
        // `QWEN_HOME` (the documented config-dir override) wins over `~/.qwen`, so a
        // test setting it redirects both detection and every write; the `qwen` binary
        // on PATH is the other signal. `QWEN_CODE` is only a per-tool-subprocess env
        // (not a whole-session marker like CC's `CLAUDECODE`), so it is not used here.
        which::which("qwen").is_ok() || user_qwen_base().is_ok_and(|b| b.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { plugins: false, mcp: true, hooks: true, scopes: &["user", "project"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        // Ownership is defined by our mcp server keys (the canonical "are we here"
        // signal); the shared probe returns Healthy — never Absent — for an mcp-less
        // plugin, so a present marker is never dropped. Source::Embedded is the only
        // steady-state source for a non-CC backend (github unsupported, path is
        // install-only), mirroring the claude probe keying on compile-time metadata.
        let comp = plugin.components(&Source::Embedded)?;
        let settings = settings_path(scope)?;
        mcpjson::probe(&settings, &["mcpServers"], &comp.mcp_servers, ServerShape::Plain)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let base = qwen_dir(scope)?;
        let settings = base.join("settings.json");

        let mut changed = false;
        changed |= mcpjson::reconcile(&settings, &["mcpServers"], &comp.mcp_servers, ServerShape::Plain)? != Outcome::NoOp;
        changed |= reconcile_hooks(&settings, &comp.hooks)?;

        // Commands copy through verbatim: qwen reads CC's own markdown+frontmatter
        // command shape, so the raw file bytes go straight under `commands/<plugin>/`.
        let cmd_root = base.join("commands").join(plugin.name);
        for doc in &comp.commands {
            changed |= write_file_idem(&cmd_root.join(command_rel(doc)), &doc.raw)?;
        }
        // Agents share `agents/` with the user's own, so we write plugin-prefixed
        // files (never a subtree we could confuse with theirs).
        let agent_root = base.join("agents");
        for doc in &comp.agents {
            changed |= write_file_idem(&agent_root.join(agent_file(plugin.name, doc)), render_agent(plugin.name, doc).as_bytes())?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let base = qwen_dir(scope)?;
        let settings = base.join("settings.json");

        let mut changed = false;
        changed |= mcpjson::remove(&settings, &["mcpServers"], &portable_names(&comp.mcp_servers))? != Outcome::NoOp;
        changed |= remove_hooks(&settings, &comp.hooks)?;

        // We own the whole `commands/<plugin>/` subtree, so a recursive drop is exact
        // and never reaches a user's own commands.
        let cmd_root = base.join("commands").join(plugin.name);
        if cmd_root.exists() {
            fs::remove_dir_all(&cmd_root).io_ctx(|| format!("removing {}", cmd_root.display()))?;
            changed = true;
        }
        // Agent files are plugin-prefixed in a shared dir, so delete only ours by name.
        let agent_root = base.join("agents");
        for doc in &comp.agents {
            changed |= remove_file_idem(&agent_root.join(agent_file(plugin.name, doc)))?;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The user config base: `$QWEN_HOME` (the documented override, the config dir
/// itself) if set, else `~/.qwen`. Honoring the override first matches what qwen
/// reads and lets a test redirect the backend without touching HOME.
fn user_qwen_base() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("QWEN_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::home_dir()
        .map(|h| h.join(".qwen"))
        .ok_or_else(|| Error::Tree("no home directory (HOME unset) and QWEN_HOME unset; cannot locate ~/.qwen".into()))
}

/// The qwen config base for a scope: the user base (above) or `<cwd>/.qwen`
/// (project). User scope needs HOME or QWEN_HOME; a missing one is a clear error.
fn qwen_dir(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => user_qwen_base(),
        Scope::Project { path } => Ok(path.join(".qwen")),
    }
}

fn settings_path(scope: &Scope) -> Result<PathBuf> {
    Ok(qwen_dir(scope)?.join("settings.json"))
}

/// Server names `reconcile` actually writes (the shared renderer skips non-portable
/// ones). `remove` keys off the same set so an unfiltered name list can never delete
/// a user server sharing a name with one we declared but never wrote (e.g. a
/// `${CLAUDE_PLUGIN_ROOT}`-bearing entry).
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
}

// --- hooks -------------------------------------------------------------------

/// Map a CC hook event to qwen-code's. qwen-code's event set is a superset of CC's
/// hook events (it adds `PostToolUseFailure`/`TodoCreated`/… of its own), so every
/// CC event maps identically. An unknown event is skipped rather than guessed.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "PreToolUse" => Some("PreToolUse"),
        "PostToolUse" => Some("PostToolUse"),
        "UserPromptSubmit" => Some("UserPromptSubmit"),
        "SessionStart" => Some("SessionStart"),
        "SessionEnd" => Some("SessionEnd"),
        "Stop" => Some("Stop"),
        "SubagentStop" => Some("SubagentStop"),
        "PreCompact" => Some("PreCompact"),
        "Notification" => Some("Notification"),
        _ => None,
    }
}

/// A `${CLAUDE_PLUGIN_ROOT}` reference only expands inside Claude Code's own hook
/// runner; qwen-code has no equivalent substitution, so such a command would spawn
/// the literal, unexpanded token. Mirrors `McpServer::is_portable` (applied locally:
/// `HookBinding` has no such method in the shared components IR).
fn hook_is_portable(hook: &HookBinding) -> bool {
    !hook.command.contains("${CLAUDE_PLUGIN_ROOT}")
}

fn render_hook_group(hook: &HookBinding) -> Value {
    let mut group = Map::new();
    if let Some(matcher) = &hook.matcher {
        group.insert("matcher".into(), Value::from(matcher.clone()));
    }
    let mut handler = Map::new();
    handler.insert("type".into(), Value::from("command"));
    handler.insert("command".into(), Value::from(hook.command.clone()));
    group.insert("hooks".into(), Value::Array(vec![Value::Object(handler)]));
    Value::Object(group)
}

/// Add-if-absent our hook groups under each mapped event in `settings.json`'s
/// `hooks` key, leaving the user's own groups in place. Idempotent: a group already
/// present (deep-equal) is not re-added. Non-portable hooks and events with no qwen
/// analog are skipped, same as mcp servers. Skips the whole edit when nothing is
/// writable so no empty `"hooks": {}` key is created for zero writes.
fn reconcile_hooks(settings: &Path, hooks: &[HookBinding]) -> Result<bool> {
    let writable: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|event| (event, h))).collect();
    if writable.is_empty() {
        return Ok(false);
    }
    json_edit(settings, |root| {
        let events = json_obj_at(root, &["hooks"]);
        for (event, hook) in &writable {
            let group = render_hook_group(hook);
            let entry = events.entry((*event).to_string()).or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(list) = entry
                && !list.iter().any(|g| g == &group)
            {
                list.push(group);
            }
        }
        Ok(())
    })
}

/// Strip exactly our hook handlers (matched by command string) from every event in
/// the `hooks` key, dropping a group or event array we emptied. A user handler
/// sharing a group with ours (or a group of their own) survives. The ownership set
/// mirrors `reconcile_hooks`'s writable filter exactly (portable AND mapped) — a
/// command string from an unmapped/non-portable hook (never written here) must never
/// be treated as ours to remove.
fn remove_hooks(settings: &Path, hooks: &[HookBinding]) -> Result<bool> {
    if !settings.exists() {
        return Ok(false);
    }
    let ours: BTreeSet<&str> =
        hooks.iter().filter(|h| hook_is_portable(h) && map_event(&h.event).is_some()).map(|h| h.command.as_str()).collect();
    json_edit(settings, |root| {
        let Some(events) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
            return Ok(());
        };
        for groups in events.values_mut() {
            let Some(list) = groups.as_array_mut() else { continue };
            for group in list.iter_mut() {
                if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                    handlers.retain(|h| h.get("command").and_then(Value::as_str).is_none_or(|c| !ours.contains(c)));
                }
            }
            list.retain(|group| group.get("hooks").and_then(Value::as_array).is_none_or(|h| !h.is_empty()));
        }
        events.retain(|_, groups| groups.as_array().is_none_or(|a| !a.is_empty()));
        Ok(())
    })
}

// --- commands / agents -------------------------------------------------------

/// `commands/hello.md` -> `hello.md`, preserving any subdir so qwen's `:`
/// namespacing (and our exact `commands/<plugin>/` removal) stays intact. Copy-
/// through: the CC command file's own bytes are what qwen reads, so no transform.
fn command_rel(doc: &MarkdownDoc) -> String {
    doc.rel.strip_prefix("commands/").unwrap_or(&doc.rel).to_string()
}

/// `agents/ez-helper.md` -> `<plugin>-ez-helper.md` (a nested path flattens). The
/// plugin prefix keeps the file identifiable as ours for an exact `remove` and clear
/// of a user's own agent of the same stem.
fn agent_file(plugin: &str, doc: &MarkdownDoc) -> String {
    format!("{plugin}-{}.md", flat_stem(&doc.rel, "agents/"))
}

fn flat_stem(rel: &str, prefix: &str) -> String {
    let stripped = rel.strip_prefix(prefix).unwrap_or(rel);
    let stem = stripped.strip_suffix(".md").unwrap_or(stripped);
    stem.replace(['/', '\\'], "-")
}

/// Render a CC agent def as a qwen subagent file. `name` is plugin-prefixed so two
/// plugins' agents never collide (qwen keys subagents by frontmatter `name`, not
/// filename); both `name` and `description` carry over JSON-quoted (a valid YAML flow
/// scalar) so a YAML-special char never breaks the frontmatter. The
/// CC `model` alias (`sonnet`/`opus`/`haiku`) is dropped — those are not qwen model
/// ids and qwen has no portable `inherit` sentinel, so the subagent falls back to
/// qwen's default model. The body (the system prompt) copies through verbatim.
/// Deterministic so a re-reconcile is byte-identical.
fn render_agent(plugin: &str, doc: &MarkdownDoc) -> String {
    let name = doc.frontmatter.get("name").and_then(Value::as_str).unwrap_or(doc.name.as_str());
    let mut out = String::from("---\n");
    // JSON-quote the full name (a valid YAML flow scalar) so a YAML-special char in
    // the plugin or agent name can't produce malformed frontmatter — same escaping
    // path as `description` below.
    out.push_str("name: ");
    out.push_str(&Value::String(format!("{plugin}-{name}")).to_string());
    out.push('\n');
    if let Some(desc) = doc.frontmatter.get("description").and_then(Value::as_str) {
        out.push_str("description: ");
        out.push_str(&Value::String(desc.to_string()).to_string());
        out.push('\n');
    }
    out.push_str("---\n\n");
    out.push_str(doc.body.trim());
    out.push('\n');
    out
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &QwenCodeBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "qwen-code detected", status: CheckStatus::Ok("`qwen` on PATH or ~/.qwen present".into()) }
    } else {
        DoctorCheck {
            name: "qwen-code detected",
            status: CheckStatus::Fail {
                problem: "qwen-code CLI not detected".into(),
                fix: "install it with `npm install -g @qwen-code/qwen-code`".into(),
            },
        }
    });

    let base = match qwen_dir(&Scope::User) {
        Ok(base) => base,
        Err(e) => {
            checks.push(DoctorCheck { name: "settings file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };
    let settings = base.join("settings.json");

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
    checks.push(check_commands_present(&comp.commands, &base.join("commands").join(plugin.name)));
    checks.push(check_agents_present(&comp.agents, &base.join("agents"), plugin.name));

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

fn check_commands_present(commands: &[MarkdownDoc], cmd_root: &Path) -> DoctorCheck {
    let name = "translated commands present";
    if commands.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no commands to translate".into()) };
    }
    let missing: Vec<String> = commands.iter().map(command_rel).filter(|rel| !cmd_root.join(rel).exists()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} command file(s) present", commands.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("command file(s) missing: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

fn check_agents_present(agents: &[MarkdownDoc], agent_root: &Path, plugin: &str) -> DoctorCheck {
    let name = "translated agents present";
    if agents.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no agents to translate".into()) };
    }
    let missing: Vec<String> = agents.iter().map(|d| agent_file(plugin, d)).filter(|f| !agent_root.join(f).exists()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} agent file(s) present", agents.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("agent file(s) missing: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/qwen_code.rs"]
mod qwen_code_tests;
