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

use serde_json::Value;

use super::cchooks::{hook_is_portable, render_hook_group};
use super::confedit::{json_edit, json_obj_at, write_file_idem};
use super::mcpjson::{self, ServerShape};
use super::report;
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc};
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

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Compose every surface (mcp + hooks in settings.json, command TOML files), so
        // a missing command file or hook group behind a healthy mcp map reads
        // NeedsRepair. `source` is the one self_heal resolved for this agent (rehydrated
        // `--path`, else the compile-time default), so probe and reconcile render
        // identical bytes.
        let comp = plugin.components(source)?;
        let base = gemini_dir(scope)?;
        let settings = base.join("settings.json");
        let mcp = mcpjson::probe_surface(&settings, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())?;
        let hooks = report::probe_json_entries(&settings, &hook_entries(&comp.hooks))?;
        let cmd_root = base.join("commands").join(plugin.name);
        let commands = report::probe_files(&expected_commands(&cmd_root, &comp.commands), |_, _| true)?;
        Ok(report::compose([mcp, hooks, commands].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let base = gemini_dir(scope)?;
        let settings = base.join("settings.json");

        let mut changed = false;
        changed |= mcpjson::reconcile(&settings, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())? != Outcome::NoOp;
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
        changed |= mcpjson::remove(&settings, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())? != Outcome::NoOp;
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

/// The `(array key_path, rendered group)` pairs `probe` checks are present under
/// `hooks.<event>`, mirroring `reconcile_hooks`'s writable filter exactly.
fn hook_entries(hooks: &[HookBinding]) -> Vec<(Vec<String>, Value)> {
    hooks
        .iter()
        .filter(|h| hook_is_portable(h))
        .filter_map(|h| map_event(&h.event).map(|event| (vec!["hooks".to_string(), event.to_string()], render_hook_group(h))))
        .collect()
}

/// The `(path, rendered bytes)` command TOML files `probe` compares against disk,
/// keyed off the same `command_rel` + `render_command_toml` `reconcile` writes.
fn expected_commands(cmd_root: &Path, commands: &[MarkdownDoc]) -> Vec<(PathBuf, Vec<u8>)> {
    commands.iter().map(|doc| (cmd_root.join(command_rel(doc)), render_command_toml(doc).into_bytes())).collect()
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

    let root = report::read_json_config(&mut checks, "settings file", &settings);

    let Some(comp) = report::components(&mut checks, plugin, source) else {
        return checks;
    };

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcpServers"],
        "not in settings.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    checks.push(check_commands_present(&comp.commands, &base.join("commands").join(plugin.name)));

    checks
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
