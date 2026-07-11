//! The gemini-cli backend: a full translate into gemini's own config. MCP goes
//! through the shared json renderer (`~/.gemini/settings.json` `mcpServers`, Plain
//! shape); hooks land in the same settings file under `hooks` with CC event names
//! mapped to gemini's; commands become one TOML file each under a plugin-named
//! subdir of `~/.gemini/commands/`. Everything we write is keyed by our plugin's
//! server names or namespaced under `<plugin>/`, so `remove` is exact and a second
//! reconcile is a true `NoOp`. Subagents/skills have no stable file surface here
//! and are skipped (see `docs/harness/gemini.md`).

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::confedit::{json_edit, json_obj_at, write_file_idem};
use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc, McpKind};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, IoContext, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct GeminiBackend;

impl AgentBackend for GeminiBackend {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn detect(&self) -> bool {
        // `~/.gemini` is HOME-based (not XDG), so a test redirecting `HOME` also
        // redirects detection; gemini has no user-config-dir override env.
        which::which("gemini").is_ok() || dirs::home_dir().is_some_and(|h| h.join(".gemini").is_dir())
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
        let base = gemini_dir(scope)?;
        let settings = base.join("settings.json");

        let mut changed = false;
        changed |= mcpjson::reconcile(&settings, &["mcpServers"], &comp.mcp_servers, ServerShape::Plain)? != Outcome::NoOp;
        changed |= reconcile_hooks(&settings, &comp.hooks)?;

        let cmd_root = base.join("commands").join(plugin.name);
        for doc in &comp.commands {
            let path = cmd_root.join(command_rel(doc));
            changed |= write_file_idem(&path, render_command_toml(doc).as_bytes())?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let base = gemini_dir(scope)?;
        let settings = base.join("settings.json");

        let mut changed = false;
        let names = portable_names(&comp.mcp_servers);
        changed |= mcpjson::remove(&settings, &["mcpServers"], &names)? != Outcome::NoOp;
        changed |= remove_hooks(&settings, &comp.hooks)?;

        // We own the whole `<commands>/<plugin>/` subtree, so a recursive drop is
        // exact and never reaches a user's own commands.
        let cmd_root = base.join("commands").join(plugin.name);
        if cmd_root.exists() {
            fs::remove_dir_all(&cmd_root).io_ctx(|| format!("removing {}", cmd_root.display()))?;
            changed = true;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The `.gemini` config base for a scope: `~/.gemini` (user) or `<cwd>/.gemini`
/// (project). User scope needs `HOME`; a missing home is a clear, actionable error.
fn gemini_dir(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => dirs::home_dir()
            .map(|h| h.join(".gemini"))
            .ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.gemini".into())),
        Scope::Project { path } => Ok(path.join(".gemini")),
    }
}

fn settings_path(scope: &Scope) -> Result<PathBuf> {
    Ok(gemini_dir(scope)?.join("settings.json"))
}

/// Server names `reconcile` actually writes (the shared renderer skips
/// non-portable ones). `remove` must key off the same set: an unfiltered name
/// list could delete an unrelated user-owned server that happens to share a
/// name with one we declared but never wrote (e.g. a
/// `${CLAUDE_PLUGIN_ROOT}`-bearing entry).
fn portable_names(servers: &[crate::components::McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
}

/// `commands/hello.md` -> `hello.toml`, preserving any subdir so gemini's `:`
/// namespacing (and our exact removal) stays intact.
fn command_rel(doc: &MarkdownDoc) -> String {
    let stripped = doc.rel.strip_prefix("commands/").unwrap_or(&doc.rel);
    let stem = stripped.strip_suffix(".md").unwrap_or(stripped);
    format!("{stem}.toml")
}

// --- hooks -------------------------------------------------------------------

/// Map a CC hook event to gemini's nearest lifecycle analog. The two verified in
/// the brief are `SessionStart` (identity) and `UserPromptSubmit` -> `BeforeAgent`;
/// the tool/compact events map by position. Events with no clean gemini counterpart
/// (`Stop`, `SubagentStop`) are skipped rather than written under a guessed name.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "SessionStart" => Some("SessionStart"),
        "SessionEnd" => Some("SessionEnd"),
        "UserPromptSubmit" => Some("BeforeAgent"),
        "PreToolUse" => Some("BeforeTool"),
        "PostToolUse" => Some("AfterTool"),
        "PreCompact" => Some("PreCompress"),
        "Notification" => Some("Notification"),
        _ => None,
    }
}

/// A `${CLAUDE_PLUGIN_ROOT}` reference only expands inside Claude Code's own hook
/// runner; gemini has no equivalent substitution, so a hook command carrying it
/// would be written verbatim and then spawn as the literal, unexpanded token.
/// Mirrors `McpServer::is_portable`; applied locally since `HookBinding` (unlike
/// `McpServer`) has no such method in the shared components IR.
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

/// Add-if-absent our hook groups under each mapped event, leaving the user's own
/// groups in place. Idempotent: a group already present (deep-equal) is not re-added.
/// Non-portable hooks (`hook_is_portable`) and events with no gemini analog
/// (`map_event` -> `None`) are skipped, same as mcp servers.
fn reconcile_hooks(settings: &Path, hooks: &[HookBinding]) -> Result<bool> {
    // Resolve to (target event, hook) up front: if nothing survives (all
    // non-portable, or all events unmapped), skip `json_edit` entirely rather
    // than let `json_obj_at` create an empty `"hooks": {}` key for zero writes.
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

/// Strip exactly our hook handlers (matched by command string) from every event,
/// dropping a group or event array we emptied. A user handler sharing a group with
/// ours (or a group of their own) survives.
fn remove_hooks(settings: &Path, hooks: &[HookBinding]) -> Result<bool> {
    if !settings.exists() {
        return Ok(false);
    }
    let ours: BTreeSet<&str> = hooks.iter().filter(|h| hook_is_portable(h)).map(|h| h.command.as_str()).collect();
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

// --- commands ----------------------------------------------------------------

/// Render a CC command doc as a gemini command TOML: frontmatter `description` ->
/// `description`, the markdown body -> `prompt`. Deterministic (so a re-reconcile
/// is byte-identical); newlines/quotes escape into single-line basic strings.
fn render_command_toml(doc: &MarkdownDoc) -> String {
    let mut out = String::new();
    if let Some(desc) = doc.frontmatter.get("description").and_then(Value::as_str) {
        out.push_str("description = ");
        out.push_str(&toml_basic_string(desc));
        out.push('\n');
    }
    out.push_str("prompt = ");
    out.push_str(&toml_basic_string(doc.body.trim()));
    out.push('\n');
    out
}

fn toml_basic_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &GeminiBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "gemini detected", status: CheckStatus::Ok("`gemini` on PATH or ~/.gemini present".into()) }
    } else {
        DoctorCheck {
            name: "gemini detected",
            status: CheckStatus::Fail {
                problem: "gemini CLI not detected".into(),
                fix: "install it with `npm install -g @google/gemini-cli`".into(),
            },
        }
    });

    let base = match gemini_dir(&Scope::User) {
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
                problem: format!("mcp server(s) not in settings.json: {}", missing.join(", ")),
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

#[cfg(test)]
#[path = "../../tests/unit/gemini.rs"]
mod gemini_tests;
