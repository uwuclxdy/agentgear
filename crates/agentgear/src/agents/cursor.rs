//! The cursor backend: a full translate into cursor's own file config. MCP goes
//! through the shared json renderer (`~/.cursor/mcp.json` `mcpServers`, Typed
//! shape — cursor requires an explicit `type:"stdio"`); hooks land in
//! `~/.cursor/hooks.json` with CC event names mapped to cursor's (`SessionStart`
//! -> `sessionStart`, `UserPromptSubmit` -> `beforeSubmitPrompt`); commands become
//! one plain-markdown file each under `~/.cursor/commands/`; CC agent defs become
//! cursor subagent files under `~/.cursor/agents/` (its native subagent surface,
//! valid at both scopes — unlike `.cursor/rules/*.mdc`, which cursor only reads as
//! project files, storing user rules in a SQLite blob no file write can reach).
//! Every file/key we write is prefixed/keyed by our plugin name, so `remove` is
//! exact and a second reconcile is a true `NoOp`. Skills have no cursor file
//! surface and are skipped (see `docs/harness/cursor.md`). Cursor is GUI-first with
//! no headless config probe, so detection rides on `~/.cursor` (or a CLI on PATH).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::confedit::{json_edit, json_obj_at, remove_file_idem, write_file_idem};
use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct CursorBackend;

impl AgentBackend for CursorBackend {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn detect(&self) -> bool {
        // The editor has no headless probe; `~/.cursor/` is the only host-agnostic
        // "configured here" signal. The `cursor-agent` CLI (or the `cursor` editor
        // shim) is a bonus, but its absence never implies the editor is absent.
        which::which("cursor-agent").is_ok()
            || which::which("cursor").is_ok()
            || dirs::home_dir().is_some_and(|h| h.join(".cursor").is_dir())
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
        mcpjson::probe(&mcp, &["mcpServers"], &comp.mcp_servers, ServerShape::typed())
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let base = cursor_dir(scope)?;

        let mut changed = false;
        changed |= mcpjson::reconcile(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, ServerShape::typed())? != Outcome::NoOp;
        changed |= reconcile_hooks(&base.join("hooks.json"), &comp.hooks)?;

        let cmd_root = base.join("commands");
        for doc in &comp.commands {
            changed |= write_file_idem(&cmd_root.join(command_file(plugin.name, doc)), command_body(doc).as_bytes())?;
        }
        let agent_root = base.join("agents");
        for doc in &comp.agents {
            changed |= write_file_idem(&agent_root.join(agent_file(plugin.name, doc)), render_agent(plugin.name, doc).as_bytes())?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let base = cursor_dir(scope)?;

        let mut changed = false;
        changed |= mcpjson::remove(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, ServerShape::typed())? != Outcome::NoOp;
        changed |= remove_hooks(&base.join("hooks.json"), &comp.hooks)?;

        // `commands/` and `agents/` are shared with the user's own files, so we
        // delete only our plugin-prefixed files by name (never a `remove_dir_all`).
        let cmd_root = base.join("commands");
        for doc in &comp.commands {
            changed |= remove_file_idem(&cmd_root.join(command_file(plugin.name, doc)))?;
        }
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

/// The `.cursor` config base for a scope: `~/.cursor` (user) or `<cwd>/.cursor`
/// (project). Cursor keys these off HOME (not XDG), so a test redirecting `HOME`
/// also redirects config + detection; user scope needs HOME set.
fn cursor_dir(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => dirs::home_dir()
            .map(|h| h.join(".cursor"))
            .ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.cursor".into())),
        Scope::Project { path } => Ok(path.join(".cursor")),
    }
}

fn mcp_path(scope: &Scope) -> Result<PathBuf> {
    Ok(cursor_dir(scope)?.join("mcp.json"))
}

// --- hooks -------------------------------------------------------------------

/// Map a CC hook event to cursor's nearest lifecycle analog. The two verified in
/// the brief are `SessionStart` -> `sessionStart` and `UserPromptSubmit` ->
/// `beforeSubmitPrompt` (fires before the prompt is sent); the tool/lifecycle
/// events map by position. Events with no clean cursor counterpart are skipped
/// rather than written under a guessed name.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "SessionStart" => Some("sessionStart"),
        "SessionEnd" => Some("sessionEnd"),
        "UserPromptSubmit" => Some("beforeSubmitPrompt"),
        "PreToolUse" => Some("preToolUse"),
        "PostToolUse" => Some("postToolUse"),
        "PreCompact" => Some("preCompact"),
        "Stop" => Some("stop"),
        "SubagentStop" => Some("subagentStop"),
        _ => None,
    }
}

/// A `${CLAUDE_PLUGIN_ROOT}` reference only expands inside Claude Code's own hook
/// runner; cursor has no equivalent substitution, so such a command would spawn as
/// the literal, unexpanded token. Mirrors `McpServer::is_portable` (applied
/// locally since `HookBinding` has no such method in the shared IR).
fn hook_is_portable(hook: &HookBinding) -> bool {
    !hook.command.contains("${CLAUDE_PLUGIN_ROOT}")
}

/// Cursor hook entries are flat `{command, matcher?}` objects directly in the
/// event array (no CC-style nested `hooks` list); `type` defaults to `"command"`.
fn render_hook_entry(hook: &HookBinding) -> Value {
    let mut obj = Map::new();
    obj.insert("command".into(), Value::from(hook.command.clone()));
    if let Some(matcher) = &hook.matcher {
        obj.insert("matcher".into(), Value::from(matcher.clone()));
    }
    Value::Object(obj)
}

/// Add-if-absent our hook entries under each mapped event, leaving the user's own
/// entries in place. Idempotent: an entry already present (deep-equal) is not
/// re-added. Non-portable hooks and events with no cursor analog are skipped; when
/// nothing survives, `json_edit` is not entered so no empty `hooks.json` is created.
fn reconcile_hooks(path: &Path, hooks: &[HookBinding]) -> Result<bool> {
    let writable: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|event| (event, h))).collect();
    if writable.is_empty() {
        return Ok(false);
    }
    json_edit(path, |root| {
        // Cursor requires a top-level `version`; set it only when absent so a
        // user's own hooks.json version survives.
        if let Value::Object(map) = root {
            map.entry("version".to_string()).or_insert_with(|| Value::from(1));
        }
        let events = json_obj_at(root, &["hooks"]);
        for (event, hook) in &writable {
            let entry = render_hook_entry(hook);
            let list = events.entry((*event).to_string()).or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(arr) = list
                && !arr.iter().any(|e| e == &entry)
            {
                arr.push(entry);
            }
        }
        Ok(())
    })
}

/// Strip exactly our hook entries (matched by command string) from every event,
/// dropping an event array we emptied. A user entry sharing an event with ours
/// survives; the file itself is left in place (merge-safe, like the mcp file).
fn remove_hooks(path: &Path, hooks: &[HookBinding]) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let ours: BTreeSet<&str> = hooks.iter().filter(|h| hook_is_portable(h)).map(|h| h.command.as_str()).collect();
    json_edit(path, |root| {
        let Some(events) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
            return Ok(());
        };
        for list in events.values_mut() {
            if let Some(arr) = list.as_array_mut() {
                arr.retain(|e| e.get("command").and_then(Value::as_str).is_none_or(|c| !ours.contains(c)));
            }
        }
        events.retain(|_, list| list.as_array().is_none_or(|a| !a.is_empty()));
        Ok(())
    })
}

// --- commands / agents -------------------------------------------------------

/// `commands/hello.md` -> `<plugin>-hello.md`. Cursor scans `commands/*.md`
/// (flat, no subdirs), so any nested path is flattened; the plugin prefix keeps
/// the file identifiable as ours for an exact `remove`.
fn command_file(plugin: &str, doc: &MarkdownDoc) -> String {
    format!("{plugin}-{}.md", flat_stem(&doc.rel, "commands/"))
}

/// `agents/ez-helper.md` -> `<plugin>-ez-helper.md`, same prefixing rule.
fn agent_file(plugin: &str, doc: &MarkdownDoc) -> String {
    format!("{plugin}-{}.md", flat_stem(&doc.rel, "agents/"))
}

fn flat_stem(rel: &str, prefix: &str) -> String {
    let stripped = rel.strip_prefix(prefix).unwrap_or(rel);
    let stem = stripped.strip_suffix(".md").unwrap_or(stripped);
    stem.replace(['/', '\\'], "-")
}

/// A cursor command file is plain markdown with no frontmatter: the whole body is
/// the injected prompt. The CC frontmatter (already split off by the IR) is
/// dropped so it never leaks into the prompt text.
fn command_body(doc: &MarkdownDoc) -> String {
    let mut body = doc.body.trim().to_string();
    body.push('\n');
    body
}

/// Render a CC agent def as a cursor subagent file (its native `agents/*.md`
/// surface). `name` is plugin-prefixed so two plugins' agents never collide;
/// `model` coerces to `inherit` (CC's `sonnet`/`opus`/`haiku` aliases are not
/// cursor model ids, and cursor documents `inherit` as the portable default).
/// Deterministic so a re-reconcile is byte-identical.
fn render_agent(plugin: &str, doc: &MarkdownDoc) -> String {
    let name = doc.frontmatter.get("name").and_then(Value::as_str).unwrap_or(doc.name.as_str());
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str("name: ");
    out.push_str(plugin);
    out.push('-');
    out.push_str(name);
    out.push('\n');
    if let Some(desc) = doc.frontmatter.get("description").and_then(Value::as_str) {
        // JSON-quote keeps a colon/quote in the description from breaking the
        // YAML scalar (JSON double-quoted strings are valid YAML flow scalars).
        out.push_str("description: ");
        out.push_str(&Value::String(desc.to_string()).to_string());
        out.push('\n');
    }
    out.push_str("model: inherit\n");
    out.push_str("---\n\n");
    out.push_str(doc.body.trim());
    out.push('\n');
    out
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &CursorBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "cursor detected", status: CheckStatus::Ok("~/.cursor present or a cursor CLI on PATH".into()) }
    } else {
        DoctorCheck {
            name: "cursor detected",
            status: CheckStatus::Fail {
                problem: "cursor not detected".into(),
                fix: "install Cursor (the editor creates ~/.cursor on first run)".into(),
            },
        }
    });

    let base = match cursor_dir(&Scope::User) {
        Ok(base) => base,
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp.json", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };
    let mcp = base.join("mcp.json");

    let root = match fs::read(&mcp) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "mcp.json", status: CheckStatus::Ok(format!("{} parses", mcp.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "mcp.json",
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
                name: "mcp.json",
                status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", mcp.display())),
            });
            None
        }
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp.json", status: CheckStatus::Warn(format!("could not read {}: {e}", mcp.display())) });
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
    checks.push(check_docs_present("translated commands present", &comp.commands, &base.join("commands"), plugin.name, "commands/"));
    checks.push(check_docs_present("translated agents present", &comp.agents, &base.join("agents"), plugin.name, "agents/"));

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

fn check_docs_present(name: &'static str, docs: &[MarkdownDoc], dir: &Path, plugin: &str, prefix: &str) -> DoctorCheck {
    if docs.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("nothing to translate".into()) };
    }
    let missing: Vec<String> =
        docs.iter().map(|d| format!("{plugin}-{}.md", flat_stem(&d.rel, prefix))).filter(|f| !dir.join(f).exists()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} file(s) present", docs.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail { problem: format!("file(s) missing: {}", missing.join(", ")), fix: "run the host's `setup`".into() },
        }
    }
}
