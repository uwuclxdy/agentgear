//! The Augment (auggie) backend: a full translate into augment's own config. Unlike
//! Claude Code, augment keeps mcpServers **and** hooks in one file — user-scope
//! `~/.augment/settings.json` — under distinct top-level keys, so both render inside
//! a single atomic read-modify-write (one `json_edit` per reconcile, never clobbering
//! the user's own keys). Hooks reuse the identical CC nested shape and augment's own
//! event names (`SessionStart`/`PreToolUse`/…); CC commands become `~/.augment/
//! commands/<plugin>-<name>.md` and CC agents `~/.augment/agents/<plugin>-<name>.md`,
//! both markdown + YAML frontmatter (augment reads the same shape as CC).
//!
//! Instructions land as a marker-tagged block in the user-scope
//! `~/.augment/rules/agentgear-<plugin>.md`: auggie recursively loads every
//! `*.md`/`*.mdc` under `~/.augment/rules/` as an always-apply rule, stripping only
//! a leading YAML block (html comments and every other byte survive verbatim), so
//! one plugin-named file in augment's own rules dir is the whole surface. Ownership
//! is the marker id, so the user's own rule files are never touched. Project scope
//! has no instructions surface, and augment's shared cross-tool guideline files are
//! never written.
//!
//! Ownership: mcp servers are keyed by our server names; command/agent files are
//! plugin-prefixed, so `remove` is exact and a second reconcile is a true `NoOp`.
//! `PreCompact`/`SubagentStop` are absent from augment's event set and skipped
//! (never guessed); skills are skipped.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::agentsmd;
use super::cchooks::{hook_is_portable, remove_hook_groups, render_hook_group};
use super::confedit::{json_edit, json_obj_at, json_prune_obj, json_remove, remove_file_idem, write_file_idem, yaml_scalar};
use super::mcpjson::{self, ServerShape};
use super::report;
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct AugmentBackend;

/// CC command frontmatter keys augment recognises; everything else is a CC-only key
/// that would be dead weight (or a parse risk) in an augment command file.
const COMMAND_KEYS: &[&str] = &["description", "argument-hint", "model"];
/// CC agent frontmatter keys augment recognises (`name` is set separately, namespaced).
const AGENT_KEYS: &[&str] = &["description", "color", "model", "tools", "disabled_tools"];

/// Augment's mcp body is the shared Plain `{command,args,env}` shape.
const SHAPE: ServerShape = ServerShape::plain();

impl AgentBackend for AugmentBackend {
    fn id(&self) -> &'static str {
        "augment"
    }

    fn detect(&self) -> bool {
        // `~/.augment` is HOME-based (not XDG), so a test redirecting `HOME` also
        // redirects detection; augment has no user-config-dir override env
        // (`AUGMENT_SESSION_AUTH` is an auth session var, not a config-dir signal).
        which::which("auggie").is_ok() || dirs::home_dir().is_some_and(|h| h.join(".augment").is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // Scope is user-only by design: the tool serves project + local too.
        // mcp + hooks + commands + agents + instructions translate; skills are skipped.
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: true,
            commands: true,
            agents: true,
            skills: false,
            instructions: true,
            scopes: &["user"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Compose every surface (mcp + hooks in settings.json, command + agent files,
        // and the rule-file guidance block), so a dropped hook group or missing
        // command/agent/block behind healthy mcp keys reads NeedsRepair. `source` is the
        // one self_heal resolved for this agent (rehydrated `--path`, else the
        // compile-time default), so probe and reconcile render identical bytes.
        let comp = plugin.components(source)?.with_client(self.id());
        let base = augment_dir(scope)?;
        let settings = base.join("settings.json");
        let mcp = mcpjson::probe_surface(&settings, &["mcpServers"], &comp.mcp_servers, SHAPE)?;
        let hooks = report::probe_json_entries(&settings, &hook_entries(&comp.hooks))?;
        let commands = report::probe_files(
            &expected_docs(&base.join("commands"), plugin.name, &comp.commands, |doc| render_command(doc).into_bytes()),
            |_, _| true,
        )?;
        let agents = report::probe_files(
            &expected_docs(&base.join("agents"), plugin.name, &comp.agents, |doc| {
                render_agent(&agent_name(plugin.name, doc), doc).into_bytes()
            }),
            |_, _| true,
        )?;
        let instructions = match instructions_file(scope, plugin.name)? {
            Some(path) => agentsmd::probe(&path, plugin)?,
            None => None,
        };
        Ok(report::compose([mcp, hooks, commands, agents, instructions].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?.with_client(self.id());
        let base = augment_dir(scope)?;

        let mut changed = false;
        changed |= reconcile_settings(&base.join("settings.json"), &comp.mcp_servers, &comp.hooks)?;

        let cmd_root = base.join("commands");
        for doc in &comp.commands {
            changed |= write_file_idem(&cmd_root.join(doc_file(plugin.name, doc)), render_command(doc).as_bytes())?;
        }
        let agent_root = base.join("agents");
        for doc in &comp.agents {
            changed |=
                write_file_idem(&agent_root.join(doc_file(plugin.name, doc)), render_agent(&agent_name(plugin.name, doc), doc).as_bytes())?;
        }
        if let Some(path) = instructions_file(scope, plugin.name)? {
            changed |= agentsmd::reconcile(&path, plugin)?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
        let comp = plugin.components(source)?.with_client(self.id());
        let base = augment_dir(scope)?;

        let mut changed = false;
        changed |= remove_from_settings(&base.join("settings.json"), &mcpjson::writable_names(&comp.mcp_servers, SHAPE), &comp.hooks)?;

        // `commands/` and `agents/` are shared with the user's own files, so we delete
        // only our plugin-prefixed files by name (never a `remove_dir_all`).
        let cmd_root = base.join("commands");
        for doc in &comp.commands {
            changed |= remove_file_idem(&cmd_root.join(doc_file(plugin.name, doc)))?;
        }
        let agent_root = base.join("agents");
        for doc in &comp.agents {
            changed |= remove_file_idem(&agent_root.join(doc_file(plugin.name, doc)))?;
        }
        // Our rule file is named for the plugin, so the removal is exact and a
        // user's own rule files under `rules/` are never touched.
        if let Some(path) = instructions_file(scope, plugin.name)? {
            changed |= agentsmd::remove(&path, plugin)?;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The `.augment` config base for a scope: `~/.augment` (user) or `<cwd>/.augment`
/// (project, defensive — capabilities declares user-only). Augment keys this off
/// HOME (not XDG), so a test redirecting `HOME` also redirects config + detection;
/// user scope needs HOME, and a missing home is a clear, actionable error.
fn augment_dir(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => dirs::home_dir()
            .map(|h| h.join(".augment"))
            .ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.augment".into())),
        Scope::Project { path } => Ok(path.join(".augment")),
    }
}

/// `commands/hello.md` -> `<plugin>-hello.md`; a nested path flattens (`a/b.md` ->
/// `<plugin>-a-b.md`). Augment scans `commands/*.md` / `agents/*.md`; the plugin
/// prefix keeps the file identifiable as ours for an exact `remove`.
fn doc_file(plugin: &str, doc: &MarkdownDoc) -> String {
    format!("{}.md", agent_name(plugin, doc))
}

/// The plugin-prefixed flat stem: an agent's frontmatter `name`, and its filename minus `.md`.
fn agent_name(plugin: &str, doc: &MarkdownDoc) -> String {
    format!("{plugin}-{}", doc.flat_stem())
}

/// The user-scope `~/.augment/rules/agentgear-<plugin>.md` rule file carrying the
/// host's always-loaded guidance: auggie recursively loads every `*.md`/`*.mdc`
/// under `~/.augment/rules/` as an always-apply rule and strips only a leading YAML
/// block (html comments and every other byte survive), so one plugin-named file in
/// augment's own rules dir is the whole surface. `None` at project scope: augment's
/// workspace guidelines are the user's own repo files, never a surface this backend
/// writes.
fn instructions_file(scope: &Scope, plugin: &str) -> Result<Option<PathBuf>> {
    match scope {
        Scope::User => Ok(Some(augment_dir(scope)?.join("rules").join(format!("agentgear-{plugin}.md")))),
        Scope::Project { .. } => Ok(None),
    }
}

// --- settings (mcp + hooks, one file) ----------------------------------------

/// Map a CC hook event to augment's. Augment's hook validator is `.strict()` over
/// exactly seven events and reuses CC's own name for six of them; `UserPromptSubmit`
/// is a straight rename to `PromptSubmit` (augment receives the prompt text on it,
/// and CC's spelling is rejected as an invalid event type). `PreCompact` and
/// `SubagentStop` are genuinely absent, so they are skipped rather than written
/// under a guessed name the validator would refuse.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "SessionStart" => Some("SessionStart"),
        "SessionEnd" => Some("SessionEnd"),
        "PreToolUse" => Some("PreToolUse"),
        "PostToolUse" => Some("PostToolUse"),
        "Stop" => Some("Stop"),
        "Notification" => Some("Notification"),
        "UserPromptSubmit" => Some("PromptSubmit"),
        _ => None,
    }
}

/// The `(array key_path, rendered group)` pairs `probe` checks are present under
/// `hooks.<event>`, mirroring `reconcile_settings`'s writable-hook filter exactly
/// (portable AND a mapped augment event).
fn hook_entries(hooks: &[HookBinding]) -> Vec<(Vec<String>, Value)> {
    hooks
        .iter()
        .filter(|h| hook_is_portable(h))
        .filter_map(|h| map_event(&h.event).map(|event| (vec!["hooks".to_string(), event.to_string()], render_hook_group(h))))
        .collect()
}

/// The `(path, rendered bytes)` files `probe` compares against disk for a surface
/// dir, keyed off the same `doc_file` + render `reconcile` writes.
fn expected_docs(dir: &Path, plugin: &str, docs: &[MarkdownDoc], render: impl Fn(&MarkdownDoc) -> Vec<u8>) -> Vec<(PathBuf, Vec<u8>)> {
    docs.iter().map(|doc| (dir.join(doc_file(plugin, doc)), render(doc))).collect()
}

/// The single settings.json write per reconcile: augment keeps mcpServers + hooks in
/// one file under distinct top-level keys, so both render inside one atomic edit.
/// mcp servers upsert by key (Plain `{command,args,env}` shape); hook groups add-if-
/// absent under each mapped event, leaving the user's own groups. Non-portable
/// servers/hooks and events with no augment analog are dropped up front; when nothing
/// is writable the edit is skipped so no empty settings.json is created for zero writes.
fn reconcile_settings(settings: &Path, servers: &[McpServer], hooks: &[HookBinding]) -> Result<bool> {
    let writable_hooks: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|event| (event, h))).collect();
    let portable_servers: Vec<&McpServer> = servers.iter().filter(|s| s.is_portable()).collect();
    if portable_servers.is_empty() && writable_hooks.is_empty() {
        return Ok(false);
    }
    json_edit(settings, |root| {
        if !portable_servers.is_empty() {
            let mcp = json_obj_at(root, &["mcpServers"]);
            for server in &portable_servers {
                if let Some(body) = mcpjson::render_server(server, SHAPE) {
                    if mcp.get(&server.name).is_some_and(|existing| mcpjson::matches_rendered(&body, existing)) {
                        continue;
                    }
                    mcp.insert(server.name.clone(), body);
                }
            }
        }
        if !writable_hooks.is_empty() {
            let events = json_obj_at(root, &["hooks"]);
            for (event, hook) in &writable_hooks {
                let group = render_hook_group(hook);
                let entry = events.entry((*event).to_string()).or_insert_with(|| Value::Array(Vec::new()));
                if let Value::Array(list) = entry
                    && !list.iter().any(|g| g == &group)
                {
                    list.push(group);
                }
            }
        }
        Ok(())
    })
}

/// Strip exactly our mcp keys + hook handlers from settings.json in one edit, leaving
/// the user's own entries. Hook ownership mirrors `reconcile_settings`'s writable
/// filter (portable AND mapped) so a command from an unmapped event — never written
/// here — is never treated as ours. A group/event array we empty is dropped, then the
/// `mcpServers`/`hooks` key our own last entry emptied, then the file itself once
/// nothing but our writes is left in it.
fn remove_from_settings(settings: &Path, server_names: &[&str], hooks: &[HookBinding]) -> Result<bool> {
    if !settings.exists() {
        return Ok(false);
    }
    let ours: BTreeSet<&str> =
        hooks.iter().filter(|h| hook_is_portable(h) && map_event(&h.event).is_some()).map(|h| h.command.as_str()).collect();
    json_remove(settings, |root| {
        json_prune_obj(root, &["mcpServers"], |mcp| {
            for name in server_names {
                mcp.remove(*name);
            }
            Ok(())
        })?;
        json_prune_obj(root, &["hooks"], |events| {
            remove_hook_groups(events, &ours);
            Ok(())
        })?;
        Ok(())
    })
}

// --- commands / agents -------------------------------------------------------

fn render_command(doc: &MarkdownDoc) -> String {
    render_doc(None, COMMAND_KEYS, doc)
}

fn render_agent(name: &str, doc: &MarkdownDoc) -> String {
    render_doc(Some(name), AGENT_KEYS, doc)
}

/// Re-emit a CC command/agent as an augment doc (same YAML-frontmatter + markdown
/// shape). `name_override` sets a namespaced, collision-free `name` (agents need one;
/// commands take their identity from the filename and pass `None`). Only augment-
/// recognised frontmatter keys (`keep`) are carried through, so a CC-only key never
/// lands in a file augment then rejects. Deterministic (BTreeMap iteration + trimmed
/// body) so a re-reconcile is byte-identical.
fn render_doc(name_override: Option<&str>, keep: &[&str], doc: &MarkdownDoc) -> String {
    let mut out = String::from("---\n");
    if let Some(name) = name_override {
        let _ = writeln!(out, "name: {}", yaml_scalar(name));
    }
    for (key, value) in &doc.frontmatter {
        if key == "name" || !keep.contains(&key.as_str()) {
            continue;
        }
        let scalar = match value {
            Value::String(s) => s.clone(),
            // The CC frontmatter parser yields strings; stringify anything else so a
            // non-scalar value can never break the document.
            other => other.to_string(),
        };
        let _ = writeln!(out, "{key}: {}", yaml_scalar(&scalar));
    }
    out.push_str("---\n\n");
    out.push_str(doc.body.trim());
    out.push('\n');
    out
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &AugmentBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "augment detected", status: CheckStatus::Ok("`auggie` on PATH or ~/.augment present".into()) }
    } else {
        DoctorCheck {
            name: "augment detected",
            status: CheckStatus::Fail {
                problem: "augment (auggie) not detected".into(),
                fix: "install it with `npm install -g @augmentcode/auggie`".into(),
            },
        }
    });

    let base = match augment_dir(&Scope::User) {
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

    let writable = mcpjson::writable_names(&comp.mcp_servers, SHAPE);
    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        &writable,
        root.as_ref(),
        &["mcpServers"],
        "not in settings.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    checks.push(check_docs_present("translated commands present", &comp.commands, &base.join("commands"), plugin.name));
    checks.push(check_docs_present("translated agents present", &comp.agents, &base.join("agents"), plugin.name));

    if plugin.instructions.is_some() {
        checks.push(agentsmd::guidance_check(instructions_file(&Scope::User, plugin.name), plugin));
    }

    checks
}

fn check_docs_present(name: &'static str, docs: &[MarkdownDoc], dir: &Path, plugin: &str) -> DoctorCheck {
    if docs.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("nothing to translate".into()) };
    }
    let missing: Vec<String> = docs.iter().map(|d| doc_file(plugin, d)).filter(|f| !dir.join(f).exists()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} file(s) present", docs.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail { problem: format!("file(s) missing: {}", missing.join(", ")), fix: "run the host's `setup`".into() },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/augment.rs"]
mod augment_tests;
