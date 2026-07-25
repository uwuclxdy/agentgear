//! The codex backend: a full translate into codex's own config, rooted at
//! `~/.codex/` (override `CODEX_HOME`). MCP is bespoke toml (`config.toml`
//! `[mcp_servers.<name>]`, via `mcptoml`); hooks land in `hooks.json` under CC's
//! exact event names (codex mirrors them 1:1); commands become flat markdown
//! prompts under `prompts/`; agents translate into codex subagent TOML under
//! `agents/`. Every mcp key is our own server name and every translated file is
//! plugin-name-prefixed, so `remove` is exact and a second reconcile is a true
//! `NoOp`.
//!
//! Two load-bearing codex caveats (see `docs/harness/codex.md`):
//! - a hook agentgear writes is INERT until a human runs codex's `/hooks` TUI and
//!   trusts its content hash — codex never fires an untrusted, non-managed hook.
//!   We still write it so a user can approve it.
//! - project scope (`<cwd>/.codex/`) is inert unless the repo path is marked
//!   trusted in the user `config.toml`; v1 is user-scope-primary and does not
//!   seed that trust entry.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::cchooks::{hook_is_portable, render_hook_group};
use super::confedit::{json_edit, json_obj_at, json_prune_obj, json_remove, remove_file_idem, write_file_idem};
use super::report;
use super::{AgentBackend, BackendState, mcptoml};
use crate::components::{HookBinding, MarkdownDoc, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct CodexBackend;

impl AgentBackend for CodexBackend {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn detect(&self) -> bool {
        which::which("codex").is_ok() || codex_home_opt().is_some_and(|d| d.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // mcp + hooks + commands (prompts) + agents translate; no skills surface.
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: true,
            commands: true,
            agents: true,
            skills: false,
            instructions: false,
            statusline: false,
            scopes: &["user", "project"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Compose every surface (mcp toml, hooks.json, prompt + agent files), so a
        // dropped hook group or missing prompt/agent behind a healthy `[mcp_servers]`
        // reads NeedsRepair. (A codex hook is inert until trusted via `/hooks`, but the
        // FILE presence is still what reconcile converges.) `source` is the one self_heal
        // resolved for this agent (rehydrated `--path`, else the compile-time default),
        // so probe and reconcile render identical bytes.
        let comp = plugin.components(source)?.with_client(self.id());
        let base = codex_base(scope)?;
        let mcp = mcptoml::probe_surface(&base.join("config.toml"), &comp.mcp_servers)?;
        let hooks = report::probe_json_entries(&base.join("hooks.json"), &hook_entries(&comp.hooks))?;
        let prompts = base.join("prompts");
        let commands = report::probe_files(
            &comp.commands.iter().map(|doc| (prompt_path(&prompts, plugin.name, doc), doc.raw.clone())).collect::<Vec<_>>(),
            |_, _| true,
        )?;
        let agents = base.join("agents");
        let agent_files = report::probe_files(
            &comp
                .agents
                .iter()
                .map(|doc| (agent_path(&agents, plugin.name, doc), render_agent_toml(plugin.name, doc).into_bytes()))
                .collect::<Vec<_>>(),
            |_, _| true,
        )?;
        Ok(report::compose([mcp, hooks, commands, agent_files].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?.with_client(self.id());
        let base = codex_base(scope)?;

        let mut changed = false;
        changed |= mcptoml::reconcile(&base.join("config.toml"), &comp.mcp_servers, desired.reenable)? != Outcome::NoOp;
        changed |= reconcile_hooks(&base.join("hooks.json"), &comp.hooks)?;

        let prompts = base.join("prompts");
        for doc in &comp.commands {
            // Copy the command markdown through verbatim: codex custom prompts are
            // markdown + YAML frontmatter (`description`/`argument-hint`), the same
            // shape as a CC command; unknown CC frontmatter keys are ignored.
            changed |= write_file_idem(&prompt_path(&prompts, plugin.name, doc), &doc.raw)?;
        }

        let agents = base.join("agents");
        for doc in &comp.agents {
            changed |= write_file_idem(&agent_path(&agents, plugin.name, doc), render_agent_toml(plugin.name, doc).as_bytes())?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
        let comp = plugin.components(source)?.with_client(self.id());
        let base = codex_base(scope)?;

        let mut changed = false;
        changed |= mcptoml::remove(&base.join("config.toml"), &portable_names(&comp.mcp_servers))? != Outcome::NoOp;
        changed |= remove_hooks(&base.join("hooks.json"), &comp.hooks)?;

        let prompts = base.join("prompts");
        for doc in &comp.commands {
            changed |= remove_file_idem(&prompt_path(&prompts, plugin.name, doc))?;
        }
        let agents = base.join("agents");
        for doc in &comp.agents {
            changed |= remove_file_idem(&agent_path(&agents, plugin.name, doc))?;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The codex home dir, honoring `CODEX_HOME` (its documented override) then
/// `~/.codex`. `_opt` never errors so `detect` can call it; a HOME-based fallback
/// means a test redirecting `HOME`/`CODEX_HOME` also redirects detection.
fn codex_home_opt() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir().map(|h| h.join(".codex"))
}

fn codex_home() -> Result<PathBuf> {
    codex_home_opt().ok_or_else(|| Error::Tree("no home directory (HOME and CODEX_HOME unset); cannot locate ~/.codex".into()))
}

/// The config base for a scope: `~/.codex` (user) or `<cwd>/.codex` (project).
fn codex_base(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => codex_home(),
        Scope::Project { path } => Ok(path.join(".codex")),
    }
}

/// `commands/hello.md` -> `<prompts>/<plugin>-hello.md`. Codex prompts are a flat
/// top-level dir (no subdirs scanned), so a nested command flattens its path into
/// the stem; the plugin prefix keeps every file identifiably ours for exact removal.
fn prompt_path(prompts: &Path, plugin: &str, doc: &MarkdownDoc) -> PathBuf {
    prompts.join(format!("{plugin}-{}.md", flat_stem(&doc.rel, "commands")))
}

/// `agents/ez-helper.md` -> `<agents>/<plugin>-ez-helper.toml`.
fn agent_path(agents: &Path, plugin: &str, doc: &MarkdownDoc) -> PathBuf {
    agents.join(format!("{plugin}-{}.toml", flat_stem(&doc.rel, "agents")))
}

/// Strip the surface dir + `.md`, flattening any remaining subdir separators to `-`.
fn flat_stem(rel: &str, subdir: &str) -> String {
    let stripped = rel.strip_prefix(subdir).unwrap_or(rel).trim_start_matches('/');
    stripped.strip_suffix(".md").unwrap_or(stripped).replace('/', "-")
}

/// Server names `reconcile` actually writes (non-portable ones are skipped).
/// `remove` keys off the same set so it never deletes a user server that happens
/// to share a name with one we declared but never wrote.
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
}

// --- agents ------------------------------------------------------------------

/// Translate a CC agent doc into a codex subagent TOML file. Codex mirrors this
/// surface (`~/.codex/agents/*.toml`, fields `name`/`description`/
/// `developer_instructions`), so a full translate lands here rather than skipping.
/// The invocation handle (`name`) is plugin-prefixed so it never collides with a
/// user's own agent and stays identifiably ours. CC's `model` alias
/// (`sonnet`/`opus`) is dropped — it has no reliable map to codex model ids, so
/// codex's own default applies. Deterministic, so a re-reconcile is byte-identical.
fn render_agent_toml(plugin: &str, doc: &MarkdownDoc) -> String {
    let name = format!("{plugin}-{}", flat_stem(&doc.rel, "agents"));
    let description = doc.frontmatter.get("description").and_then(Value::as_str).unwrap_or_default();
    let mut out = String::new();
    let _ = writeln!(out, "name = {}", toml_basic_string(&name));
    let _ = writeln!(out, "description = {}", toml_basic_string(description));
    out.push_str("developer_instructions = ");
    out.push_str(&toml_multiline_basic(doc.body.trim()));
    out.push('\n');
    out
}

/// A single-line TOML basic string, escaping so an arbitrary value is safe.
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
            // TOML basic strings require every control char except tab to be
            // escaped, including DEL (U+007F) — not just the sub-0x20 range.
            c if (c as u32) < 0x20 || c as u32 == 0x7F => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A multiline TOML basic string (`"""..."""`) for the agent body: only `\` and a
/// literal `"""` need escaping, so newlines stay literal and the file reads
/// naturally. The newline right after the opening delimiter is trimmed by TOML.
fn toml_multiline_basic(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace("\"\"\"", "\"\"\\\"");
    let mut out = String::with_capacity(escaped.len() + 8);
    out.push_str("\"\"\"\n");
    out.push_str(&escaped);
    if !escaped.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\"\"\"");
    out
}

// --- hooks -------------------------------------------------------------------

/// Codex's hook event names match Claude Code's 1:1 (a deliberate cross-CLI
/// convention), so a supported CC event passes through unchanged; an event codex
/// does not define is skipped rather than written under a guessed name.
const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "SubagentStart",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PreCompact",
    "PostCompact",
    "UserPromptSubmit",
    "SubagentStop",
    "Stop",
];

fn map_event(cc_event: &str) -> Option<&'static str> {
    CODEX_EVENTS.iter().copied().find(|e| *e == cc_event)
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

/// Add-if-absent our hook groups into codex's `hooks.json` (CC's exact shape, event
/// names identical), leaving the user's own groups. Idempotent: a deep-equal group
/// is not re-added. LOUD CAVEAT: what we write is inert until a human runs codex's
/// `/hooks` TUI and trusts it (see the module doc + `docs/harness/codex.md`).
fn reconcile_hooks(hooks_json: &Path, hooks: &[HookBinding]) -> Result<bool> {
    let writable: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|e| (e, h))).collect();
    if writable.is_empty() {
        return Ok(false);
    }
    json_edit(hooks_json, |root| {
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
fn remove_hooks(hooks_json: &Path, hooks: &[HookBinding]) -> Result<bool> {
    if !hooks_json.exists() {
        return Ok(false);
    }
    let ours: BTreeSet<&str> = hooks.iter().filter(|h| hook_is_portable(h)).map(|h| h.command.as_str()).collect();
    json_remove(hooks_json, |root| {
        json_prune_obj(root, &["hooks"], |events| {
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
        .map(|_| ())
    })
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &CodexBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "codex detected", status: CheckStatus::Ok("`codex` on PATH or ~/.codex present".into()) }
    } else {
        DoctorCheck {
            name: "codex detected",
            status: CheckStatus::Fail {
                problem: "codex CLI not detected".into(),
                fix: "install it with `npm install -g @openai/codex`".into(),
            },
        }
    });

    let base = match codex_base(&Scope::User) {
        Ok(base) => base,
        Err(e) => {
            checks.push(DoctorCheck { name: "config file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };
    let config = base.join("config.toml");

    let doc = match fs::read_to_string(&config) {
        Ok(text) => match text.parse::<toml_edit::DocumentMut>() {
            Ok(doc) => {
                checks.push(DoctorCheck { name: "config file", status: CheckStatus::Ok(format!("{} parses", config.display())) });
                Some(doc)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "config file",
                    status: CheckStatus::Fail {
                        problem: format!("{} does not parse: {e}", config.display()),
                        fix: "fix the TOML syntax or remove the file".into(),
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

    let Some(comp) = report::components(&mut checks, plugin, source).map(|c| c.with_client(backend.id())) else {
        return checks;
    };

    checks.push(check_mcp_registered(&comp.mcp_servers, doc.as_ref()));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    checks.push(check_prompts_present(&comp.commands, &base.join("prompts"), plugin.name));
    checks.push(check_agents_present(&comp.agents, &base.join("agents"), plugin.name));
    checks.push(check_hooks_present(&comp.hooks, &base.join("hooks.json")));

    checks
}

fn check_mcp_registered(servers: &[McpServer], doc: Option<&toml_edit::DocumentMut>) -> DoctorCheck {
    let name = "mcp server registered";
    let portable: Vec<&str> = servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect();
    let skipped = report::skipped_mcp(servers, &portable);
    if portable.is_empty() {
        return report::note_skipped(DoctorCheck { name, status: CheckStatus::Ok(report::NO_MCP.into()) }, &skipped);
    }
    let table = doc.and_then(|d| d.get("mcp_servers")).and_then(toml_edit::Item::as_table);
    let missing: Vec<&str> = portable.iter().copied().filter(|n| table.is_none_or(|t| !t.contains_key(n))).collect();
    if !missing.is_empty() {
        return DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp server(s) not in config.toml [mcp_servers]: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        };
    }
    report::note_skipped(DoctorCheck { name, status: CheckStatus::Ok(format!("{} registered", portable.join(", "))) }, &skipped)
}

fn check_prompts_present(commands: &[MarkdownDoc], prompts: &Path, plugin: &str) -> DoctorCheck {
    let name = "translated prompts present";
    if commands.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no commands to translate".into()) };
    }
    let missing: Vec<String> =
        commands.iter().map(|doc| prompt_path(prompts, plugin, doc)).filter(|p| !p.exists()).map(|p| p.display().to_string()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} prompt file(s) present", commands.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("prompt file(s) missing: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

fn check_agents_present(agents: &[MarkdownDoc], agents_dir: &Path, plugin: &str) -> DoctorCheck {
    let name = "translated agents present";
    if agents.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no agents to translate".into()) };
    }
    let missing: Vec<String> =
        agents.iter().map(|doc| agent_path(agents_dir, plugin, doc)).filter(|p| !p.exists()).map(|p| p.display().to_string()).collect();
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

/// Hooks are present-but-inert: a `Warn`, not an `Ok`, surfaces the codex trust gate
/// (they never fire until approved via `/hooks`) without failing an otherwise-healthy
/// report. A missing hook we should have written is a real `Fail`.
fn check_hooks_present(hooks: &[HookBinding], hooks_json: &Path) -> DoctorCheck {
    let name = "translated hooks present";
    let skipped = report::skipped_hooks(hooks);
    let ours: Vec<&str> =
        hooks.iter().filter(|h| hook_is_portable(h) && map_event(&h.event).is_some()).map(|h| h.command.as_str()).collect();
    if ours.is_empty() {
        return report::note_skipped(DoctorCheck { name, status: CheckStatus::Ok("no hooks to translate".into()) }, &skipped);
    }
    let text = fs::read_to_string(hooks_json).unwrap_or_default();
    let missing: Vec<&str> = ours.iter().copied().filter(|c| !text.contains(c)).collect();
    let check = if missing.is_empty() {
        DoctorCheck {
            name,
            status: CheckStatus::Warn(format!("present in {} but INERT until trusted via codex `/hooks`", hooks_json.display())),
        }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("hook(s) missing from hooks.json: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    };
    report::note_skipped(check, &skipped)
}

#[cfg(test)]
#[path = "../../tests/unit/codex.rs"]
mod codex_tests;
