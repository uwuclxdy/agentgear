//! The gemini-cli backend: a full translate into gemini's own config. MCP goes
//! through the shared json renderer (`~/.gemini/settings.json` `mcpServers`, Plain
//! shape); hooks land in the same settings file under `hooks` with CC event names
//! mapped to gemini's; commands become one TOML file each under a plugin-named
//! subdir of `~/.gemini/commands/`; CC agents become plugin-prefixed
//! `~/.gemini/agents/<plugin>-<name>.md` subagent files, gemini's stable general
//! subagent-file schema (not the still-preview extension-bundled form — see
//! `docs/harness/gemini.md`). Everything we write is keyed by our plugin's server
//! names, namespaced under `<plugin>/`, or plugin-prefixed, so `remove` is exact
//! and a second reconcile is a true `NoOp`. Skills have no stable file surface
//! here and are skipped (see `docs/harness/gemini.md`).

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::cchooks::{hook_is_portable, remove_hook_groups, render_hook_group};
use super::confedit::{json_edit, json_obj_at, json_prune_obj, json_remove, remove_file_idem, write_file_idem, yaml_scalar};
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
        // mcp + hooks + commands + agents translate; skills have no stable surface.
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: true,
            commands: true,
            agents: true,
            skills: false,
            instructions: false,
            scopes: &["user", "project"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Compose every surface (mcp + hooks in settings.json, command + agent
        // files), so a missing command/agent file or hook group behind a healthy
        // mcp map reads NeedsRepair. `source` is the one self_heal resolved for
        // this agent (rehydrated `--path`, else the compile-time default), so
        // probe and reconcile render identical bytes.
        let comp = plugin.components(source)?.with_client(self.id());
        let base = gemini_dir(scope)?;
        let settings = base.join("settings.json");
        let mcp = mcpjson::probe_surface(&settings, &["mcpServers"], &comp.mcp_servers, ServerShape::plain())?;
        let hooks = report::probe_json_entries(&settings, &hook_entries(&comp.hooks))?;
        let cmd_root = base.join("commands").join(plugin.name);
        let commands = report::probe_files(&expected_commands(&cmd_root, &comp.commands), |_, _| true)?;
        let agent_root = base.join("agents");
        let agents = report::probe_files(&expected_agents(&agent_root, plugin.name, &comp.agents), |_, _| true)?;
        Ok(report::compose([mcp, hooks, commands, agents].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?.with_client(self.id());
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
        // Agents share `agents/` with the user's own, so we write plugin-prefixed
        // files (never a subtree we could confuse with theirs).
        let agent_root = base.join("agents");
        for doc in &comp.agents {
            changed |= write_file_idem(&agent_root.join(agent_file(plugin.name, doc)), render_agent(plugin.name, doc).as_bytes())?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
        let comp = plugin.components(source)?.with_client(self.id());
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

/// Map a CC hook event to gemini's nearest lifecycle analog. `SessionStart` is
/// identity, `UserPromptSubmit` -> `BeforeAgent`, tool/compact map by position.
/// `Stop` and the subagent pair (`SubagentStart`/`SubagentStop`) return `None`:
/// gemini's 11-event `HookEventName` enum carries no subagent event, and its lone
/// agent-lifecycle analog `AfterAgent` would over-fire on every main-agent turn.
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

// --- agents --------------------------------------------------------------------

/// `agents/ez-helper.md` -> `<plugin>-ez-helper.md` (a nested path flattens). The
/// plugin prefix keeps the file identifiable as ours for an exact `remove` and
/// clear of a user's own agent of the same stem.
fn agent_file(plugin: &str, doc: &MarkdownDoc) -> String {
    format!("{plugin}-{}.md", flat_stem(&doc.rel, "agents/"))
}

fn flat_stem(rel: &str, prefix: &str) -> String {
    let stripped = rel.strip_prefix(prefix).unwrap_or(rel);
    let stem = stripped.strip_suffix(".md").unwrap_or(stripped);
    stem.replace(['/', '\\'], "-")
}

/// Render a CC agent def as a gemini subagent file: gemini's stable general
/// subagent schema requires `name` (a slug) + `description` in YAML frontmatter
/// (docs/harness/gemini.md); the optional fields (`kind`/`tools`/`mcpServers`/
/// `model`/`temperature`/`max_turns`/`timeout_mins`) are left to gemini's own
/// defaults since the CC IR carries none of them. `name` is plugin-prefixed so two
/// plugins' agents never collide (gemini keys subagents by frontmatter `name`, not
/// filename). The CC `model` alias (`sonnet`/`opus`/`haiku`) is dropped — those
/// are not gemini model ids and gemini has no portable `inherit` sentinel, so the
/// subagent falls back to gemini's default model. The body (the system prompt)
/// copies through verbatim. Deterministic so a re-reconcile is byte-identical.
fn render_agent(plugin: &str, doc: &MarkdownDoc) -> String {
    let name = doc.frontmatter.get("name").and_then(Value::as_str).unwrap_or(doc.name.as_str());
    let mut out = String::from("---\n");
    let _ = writeln!(out, "name: {}", yaml_scalar(&format!("{plugin}-{name}")));
    if let Some(desc) = doc.frontmatter.get("description").and_then(Value::as_str) {
        let _ = writeln!(out, "description: {}", yaml_scalar(desc));
    }
    out.push_str("---\n\n");
    out.push_str(doc.body.trim());
    out.push('\n');
    out
}

/// The `(path, rendered bytes)` agent files `probe` compares against disk, keyed
/// off the same `agent_file` + `render_agent` `reconcile` writes.
fn expected_agents(agent_root: &Path, plugin: &str, agents: &[MarkdownDoc]) -> Vec<(PathBuf, Vec<u8>)> {
    agents.iter().map(|doc| (agent_root.join(agent_file(plugin, doc)), render_agent(plugin, doc).into_bytes())).collect()
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
    json_remove(settings, |root| {
        json_prune_obj(root, &["hooks"], |events| {
            remove_hook_groups(events, &ours);
            Ok(())
        })
        .map(|_| ())
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

    let Some(comp) = report::components(&mut checks, plugin, source).map(|c| c.with_client(backend.id())) else {
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
    checks.push(check_agents_present(&comp.agents, &base.join("agents"), plugin.name));

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
#[path = "../../tests/unit/gemini.rs"]
mod gemini_tests;
