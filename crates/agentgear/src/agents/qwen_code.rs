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

use serde_json::Value;

use super::cchooks::{hook_is_portable, render_hook_group};
use super::confedit::{json_edit, json_obj_at, remove_file_idem, write_file_idem};
use super::mcpjson::{self, RemoteShape, ServerShape};
use super::report;
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, IoContext, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct QwenCodeBackend;

/// qwen picks transport purely by key presence (`httpUrl` = http, `url` = sse;
/// `type` is never read), so a `url`-keyed http server would silently load over SSE.
const SHAPE: ServerShape = ServerShape::plain().with_remote(RemoteShape::HttpUrlKeyed);

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
        // Compose every surface (mcp + hooks in settings.json, command + agent files),
        // so a dropped hook group or missing command/agent behind healthy mcp keys reads
        // NeedsRepair. Source::Embedded is the only steady-state source for a non-CC
        // backend (github unsupported, path install-only).
        let comp = plugin.components(&Source::Embedded)?;
        let base = qwen_dir(scope)?;
        let settings = base.join("settings.json");
        let mcp = mcpjson::probe_surface(&settings, &["mcpServers"], &comp.mcp_servers, SHAPE)?;
        let hooks = report::probe_json_entries(&settings, &hook_entries(&comp.hooks))?;
        let cmd_root = base.join("commands").join(plugin.name);
        let commands = report::probe_files(
            &comp.commands.iter().map(|doc| (cmd_root.join(command_rel(doc)), doc.raw.clone())).collect::<Vec<_>>(),
            |_, _| true,
        )?;
        let agent_root = base.join("agents");
        let agents = report::probe_files(
            &comp
                .agents
                .iter()
                .map(|doc| (agent_root.join(agent_file(plugin.name, doc)), render_agent(plugin.name, doc).into_bytes()))
                .collect::<Vec<_>>(),
            |_, _| true,
        )?;
        Ok(report::compose([mcp, hooks, commands, agents].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let base = qwen_dir(scope)?;
        let settings = base.join("settings.json");

        let mut changed = false;
        changed |= mcpjson::reconcile(&settings, &["mcpServers"], &comp.mcp_servers, SHAPE)? != Outcome::NoOp;
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
        changed |= mcpjson::remove(&settings, &["mcpServers"], &comp.mcp_servers, SHAPE)? != Outcome::NoOp;
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

/// The `(array key_path, rendered group)` pairs `probe` checks are present under
/// `hooks.<event>`, mirroring `reconcile_hooks`'s writable filter exactly.
fn hook_entries(hooks: &[HookBinding]) -> Vec<(Vec<String>, Value)> {
    hooks
        .iter()
        .filter(|h| hook_is_portable(h))
        .filter_map(|h| map_event(&h.event).map(|event| (vec!["hooks".to_string(), event.to_string()], render_hook_group(h))))
        .collect()
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
#[path = "../../tests/unit/qwen_code.rs"]
mod qwen_code_tests;
