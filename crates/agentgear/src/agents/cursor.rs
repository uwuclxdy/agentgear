//! The cursor backend: a full translate into cursor's own file config. MCP goes
//! through the shared json renderer (`~/.cursor/mcp.json` `mcpServers`, Typed
//! shape — cursor requires an explicit `type:"stdio"`); hooks land in
//! `~/.cursor/hooks.json` with CC event names mapped to cursor's (`SessionStart`
//! -> `sessionStart`, `UserPromptSubmit` -> `beforeSubmitPrompt`); commands become
//! one plain-markdown file each under `~/.cursor/commands/`; CC agent defs become
//! cursor subagent files under `~/.cursor/agents/` (its native subagent surface,
//! valid at both scopes). Every file/key we write is prefixed/keyed by our plugin
//! name, so `remove` is exact and a second reconcile is a true `NoOp`. Skills land
//! as bare `~/.cursor/skills/<name>/SKILL.md` (both scopes), tagged for ownership
//! so `remove` only ever deletes skills we wrote. Cursor is GUI-first with no
//! headless config probe, so detection rides on `~/.cursor` (or a CLI on PATH).
//!
//! The host's always-loaded guidance lands as one `.cursor/rules/*.mdc` rule per
//! scope: cursor's agent session reads `.cursor/rules/**/*.mdc` for the workspace
//! and additionally walks every ancestor of the workspace root up to `/` (each
//! level's `.cursor/rules/*.mdc` and `AGENTS.md`), so a rule in `~/.cursor/rules/`
//! reaches every project under HOME — cursor's own rule UI writes its user rules
//! exactly there, and the old SQLite `state.vscdb` global-rules store is gone from
//! the CLI bundle. `alwaysApply: true` is the one frontmatter class cursor loads
//! unconditionally (its globs/description triage is skipped), so the rule carries
//! that plus the same frontmatter-scoped `x-agentgear` tag as the skills surface:
//! the tag, not the file name, decides write/repair/skip, a user-authored file at
//! our exact name is never clobbered or swept, and `remove` takes the `rules/` dir
//! only when our own removal emptied it. The legacy `.cursorrules` file is also
//! force-always-on, but its single shared slot makes it never a target. No
//! auth-free channel observes cursor loading rules live; the read paths above are
//! bundle-read facts.
//!
//! **The whole translation is retired when cursor already surfaces the plugin itself.**
//! Cursor's default-on `loadClaude` loader reads CC's own on-disk registry
//! (`$HOME/.claude/plugins/installed_plugins.json`,
//! HOME-based, hardcoded) and, for each listed+enabled plugin, loads
//! `.claude-plugin/plugin.json` straight off its `installPath` — mcp, hooks, skills,
//! rules, agents, commands, the **whole** surface, with `${CLAUDE_PLUGIN_ROOT}`
//! expansion. So once the `claude` backend has us registered there, translating any of
//! it ourselves would double-register every surface, not just agents (contrast omp,
//! which only doubles on agents). `cc_registry_covers` (`super::ccregistry::
//! registry_lists_plugin`, shared with omp's own gate) retires `probe`/`reconcile`
//! entirely: no surface is checked or written, `probe` reads `Healthy` (nothing owned,
//! marker kept — see `report::compose`'s "no surface contributed" rule), and
//! `reconcile` is a true `NoOp`. `remove` stays unconditional, so a plugin translated
//! before the `claude` backend was ever installed still tears down cleanly. Unlike omp,
//! cursor has no own-side enable toggle to also check: the gate is exactly "CC's
//! registry lists us," matching the shape of omp's "installed" half without omp's
//! provider half (cursor's `loadClaude` also reads CC's `enabledPlugins`, but per the
//! `no enable-state guessing` design principle and to keep the two gates one signal, we
//! deliberately don't model that: translating an extra time on a CC-side-disabled
//! plugin is the conservative failure, not silently losing cursor's coverage). The
//! registry read is HOME-based, matching cursor's own hardcoded `join(HOME, ".claude")`
//! resolution (it ignores `CLAUDE_CONFIG_DIR`), so a relocated config dir moves the
//! registry off this path and the translation fallback fires.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::cchooks::hook_is_portable;
use super::ccregistry::registry_lists_plugin;
use super::confedit::{json_edit, json_obj_at, json_prune_obj, json_remove, remove_file_idem, write_file_idem, yaml_scalar};
use super::mcpjson::{self, ServerShape};
use super::report;
use super::skillsdir;
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, IoContext, Result};
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
        // Whole surface translates (mcp + hooks + commands + agents + skills +
        // instructions); the cc-registry `loadClaude` coverage is a runtime retire
        // gate, not a capability.
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: true,
            commands: true,
            agents: true,
            skills: true,
            instructions: true,
            scopes: &["user", "project"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Cursor's own `loadClaude` loader already covers every surface off CC's
        // registry (see the module doc): we own nothing on disk for this plugin, so
        // this reads `Healthy` (not `Absent`) exactly like `report::compose` folding
        // zero contributed surfaces — the marker is kept, self_heal never churns it.
        if cc_registry_covers(plugin) {
            return Ok(BackendState::Healthy);
        }
        // Compose every surface (mcp.json, hooks.json, command + agent files), so a
        // dropped hook entry or missing command/agent behind a healthy mcp.json reads
        // NeedsRepair. `source` is the one self_heal resolved for this agent (rehydrated
        // `--path`, else the compile-time default), so probe and reconcile render
        // identical bytes.
        let comp = plugin.components(source)?.with_client(self.id());
        let base = cursor_dir(scope)?;
        let mcp = mcpjson::probe_surface(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, ServerShape::typed())?;
        let hooks = report::probe_json_entries(&base.join("hooks.json"), &hook_entries(&comp.hooks))?;
        let commands = report::probe_files(
            &expected_docs(&base.join("commands"), plugin.name, "commands/", &comp.commands, |doc| command_body(doc).into_bytes()),
            |_, _| true,
        )?;
        let agents = report::probe_files(
            &expected_docs(&base.join("agents"), plugin.name, "agents/", &comp.agents, |doc| render_agent(plugin.name, doc).into_bytes()),
            |_, _| true,
        )?;
        let skills = skillsdir::probe(&base.join("skills"), plugin, &comp.skills)?;
        let instructions = probe_instructions(&base, plugin)?;
        Ok(report::compose([mcp, hooks, commands, agents, skills, instructions].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        // See the module doc: cursor's `loadClaude` loader already covers every
        // surface off CC's registry, so translating any of it ourselves would
        // double-register the whole plugin, not just one surface.
        if cc_registry_covers(plugin) {
            return Ok(Outcome::NoOp);
        }
        let comp = plugin.components(&desired.source)?.with_client(self.id());
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
        changed |= skillsdir::reconcile(&base.join("skills"), plugin, &comp.skills)?;
        changed |= reconcile_instructions(&base, plugin)?;
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
        let comp = plugin.components(source)?.with_client(self.id());
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
        changed |= skillsdir::remove(&base.join("skills"), plugin, &comp.skills)?;
        changed |= remove_instructions(&base, plugin)?;
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

// --- cc-registry retire gate --------------------------------------------------

/// True when cursor's own `loadClaude` loader already surfaces this plugin's whole
/// component tree off Claude Code's on-disk registry, so translating any of it
/// ourselves would double-register every surface. See the module doc for why this
/// is a single "installed" check with no omp-style provider half. The read is
/// HOME-based, matching cursor's own hardcoded `join(HOME, ".claude")` resolution
/// (unaffected by `CLAUDE_CONFIG_DIR`), so a relocated config dir reads as
/// not-covered and the translation fallback fires.
fn cc_registry_covers(plugin: &Plugin) -> bool {
    let Some(cc) = dirs::home_dir().map(|h| h.join(".claude")) else {
        return false;
    };
    registry_lists_plugin(&cc.join("plugins").join("installed_plugins.json"), &plugin.id())
}

// --- hooks -------------------------------------------------------------------

/// Map a CC hook event to cursor's nearest lifecycle analog. `SessionStart` ->
/// `sessionStart` and `UserPromptSubmit` -> `beforeSubmitPrompt` (fires before the
/// prompt is sent); the tool/lifecycle events map by position. Both subagent events
/// map: cursor's agent-hook catalog carries `subagentStart` and `subagentStop`.
/// Events with no clean cursor counterpart are skipped rather than written under a
/// guessed name.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "SessionStart" => Some("sessionStart"),
        "SessionEnd" => Some("sessionEnd"),
        "UserPromptSubmit" => Some("beforeSubmitPrompt"),
        "PreToolUse" => Some("preToolUse"),
        "PostToolUse" => Some("postToolUse"),
        "PreCompact" => Some("preCompact"),
        "Stop" => Some("stop"),
        "SubagentStart" => Some("subagentStart"),
        "SubagentStop" => Some("subagentStop"),
        _ => None,
    }
}

/// The `(array key_path, rendered entry)` pairs `probe` checks are present under
/// `hooks.<event>`, mirroring `reconcile_hooks`'s writable filter exactly.
fn hook_entries(hooks: &[HookBinding]) -> Vec<(Vec<String>, Value)> {
    hooks
        .iter()
        .filter(|h| hook_is_portable(h))
        .filter_map(|h| map_event(&h.event).map(|event| (vec!["hooks".to_string(), event.to_string()], render_hook_entry(h))))
        .collect()
}

/// The `(path, rendered bytes)` files `probe` compares against disk for a surface
/// dir; the file name is `<plugin>-<flat stem>.md`, matching `command_file`/`agent_file`.
fn expected_docs(
    dir: &Path, plugin: &str, prefix: &str, docs: &[MarkdownDoc], render: impl Fn(&MarkdownDoc) -> Vec<u8>,
) -> Vec<(PathBuf, Vec<u8>)> {
    docs.iter().map(|doc| (dir.join(format!("{plugin}-{}.md", flat_stem(&doc.rel, prefix))), render(doc))).collect()
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
/// dropping only an event array OUR removal emptied. A user entry sharing an event
/// with ours survives, and so does an event array they already had empty. Cursor's
/// entries sit directly in the event array (no CC group wrapper), so this is
/// [`super::cchooks::remove_hook_groups`]'s rule with one level instead of two.
fn remove_hooks(path: &Path, hooks: &[HookBinding]) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let ours: BTreeSet<&str> = hooks.iter().filter(|h| hook_is_portable(h)).map(|h| h.command.as_str()).collect();
    json_remove(path, |root| {
        json_prune_obj(root, &["hooks"], |events| {
            events.retain(|_, list| {
                let Some(arr) = list.as_array_mut() else { return true };
                if arr.is_empty() {
                    return true;
                }
                arr.retain(|e| e.get("command").and_then(Value::as_str).is_none_or(|c| !ours.contains(c)));
                !arr.is_empty()
            });
            Ok(())
        })
        .map(|_| ())
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

// --- instructions (always-apply rule) -----------------------------------------

/// Frontmatter key naming the owning `<plugin>@<marketplace>` id, the same tag the
/// skills surface uses. Presence with our exact value marks a rule file as ours;
/// the tag, never the file name, carries ownership.
const TAG_KEY: &str = "x-agentgear";

/// Our rule file under a `.cursor` base's `rules/` dir. The `agentgear-` prefix
/// makes the file identifiable as ours at a glance; ownership itself rides the
/// frontmatter tag.
fn instructions_path(base: &Path, plugin: &Plugin) -> PathBuf {
    base.join("rules").join(format!("agentgear-{}.mdc", plugin.name))
}

/// The rule's exact bytes: cursor frontmatter whose `alwaysApply: true` puts it in
/// the unconditional class, our ownership tag, then the guidance text with a
/// trailing newline guaranteed. The one render both reconcile and probe key off,
/// so the two cannot drift.
fn render_instructions(plugin: &Plugin, text: &str) -> Vec<u8> {
    let mut out = format!("---\nalwaysApply: true\n{TAG_KEY}: {}\n---\n", yaml_scalar(&plugin.id()));
    out.push_str(text);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.into_bytes()
}

/// Write or repair our rule file. A slot already occupied by a foreign file is
/// skipped whole — never clobbered, never tagged, exactly like a foreign
/// same-named skill. `None` instructions -> no-op. Returns whether anything
/// changed (a converged file writes zero bytes).
fn reconcile_instructions(base: &Path, plugin: &Plugin) -> Result<bool> {
    let Some(text) = &plugin.instructions else { return Ok(false) };
    let path = instructions_path(base, plugin);
    if slot_ownership(&path, plugin)? == Some(false) {
        return Ok(false);
    }
    write_file_idem(&path, &render_instructions(plugin, text))
}

/// Classify the instructions surface for `probe`: `None` when the plugin declares
/// no guidance or the slot is foreign (contributes nothing to the composite),
/// else the file's state — `Absent` (a deleted rule self-heals back in),
/// `Healthy` (byte-exact), `NeedsRepair` (tagged but drifted). Reuses
/// [`report::probe_files`] with the tag as the drift filter.
fn probe_instructions(base: &Path, plugin: &Plugin) -> Result<Option<BackendState>> {
    let Some(text) = &plugin.instructions else { return Ok(None) };
    let tag = plugin.id();
    report::probe_files(&[(instructions_path(base, plugin), render_instructions(plugin, text))], |_, existing| {
        std::str::from_utf8(existing).is_ok_and(|t| frontmatter_has_tag(t, &tag))
    })
}

/// Strip our rule file whenever it carries our tag — unconditional, like every
/// other cursor surface (the cc-registry retire gate pauses reconcile/probe only).
/// The `rules/` dir goes only when our own removal emptied it, so a foreign
/// sibling rule survives with the dir.
fn remove_instructions(base: &Path, plugin: &Plugin) -> Result<bool> {
    let rules = base.join("rules");
    let path = instructions_path(base, plugin);
    if slot_ownership(&path, plugin)? != Some(true) {
        return Ok(false);
    }
    let removed = remove_file_idem(&path)?;
    if removed && fs::read_dir(&rules).io_ctx(|| format!("reading {}", rules.display()))?.next().is_none() {
        fs::remove_dir(&rules).io_ctx(|| format!("removing {}", rules.display()))?;
    }
    Ok(removed)
}

/// Ownership of our rule slot: `None` when absent (writable), `Some(true)` ours,
/// `Some(false)` foreign. Shared by reconcile (skip a foreign slot), remove
/// (delete only ours) and probe's drift filter, so the three can never disagree.
fn slot_ownership(path: &Path, plugin: &Plugin) -> Result<Option<bool>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(frontmatter_has_tag(&text, &plugin.id()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io { context: format!("reading {}", path.display()), source }),
    }
}

/// Whether a rule file's leading `---`-fenced frontmatter carries a top-level
/// `x-agentgear` key with exactly our plugin id, the value form our render writes.
/// Scoped to the frontmatter block, never a whole-file substring: a tag line
/// quoted in the rule body is not ours (skillsdir's discipline). An unterminated
/// fence reads as no frontmatter.
fn frontmatter_has_tag(text: &str, tag: &str) -> bool {
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return false;
    };
    let mut ours = false;
    let mut terminated = false;
    for line in rest.split_inclusive('\n') {
        let line = line.trim_end_matches(['\n', '\r']);
        if line == "---" {
            terminated = true;
            break;
        }
        if !line.starts_with([' ', '\t'])
            && let Some((key, value)) = line.split_once(':')
            && key.trim() == TAG_KEY
            && value.trim() == yaml_scalar(tag)
        {
            ours = true;
        }
    }
    terminated && ours
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

    if cc_registry_covers(plugin) {
        checks.push(DoctorCheck {
            name: "translation",
            status: CheckStatus::Ok("covered by Claude Code's own `loadClaude` plugin registry; not translated".into()),
        });
        return checks;
    }

    let base = match cursor_dir(&Scope::User) {
        Ok(base) => base,
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp.json", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };
    let mcp = base.join("mcp.json");

    let root = report::read_json_config(&mut checks, "mcp.json", &mcp);

    let Some(comp) = report::components(&mut checks, plugin, source).map(|c| c.with_client(backend.id())) else {
        return checks;
    };

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcpServers"],
        "not in mcp.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    checks.push(check_docs_present("translated commands present", &comp.commands, &base.join("commands"), plugin.name, "commands/"));
    checks.push(check_docs_present("translated agents present", &comp.agents, &base.join("agents"), plugin.name, "agents/"));

    if plugin.instructions.is_some() {
        checks.push(instructions_check(&base, plugin));
    }

    checks
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

/// The doctor check for the instructions surface, keyed on our own rule file (the
/// same semantics as `agentsmd::guidance_check`): present and byte-exact -> Ok;
/// absent or drifted -> fail naming the fix; a slot held by a foreign (untagged)
/// file -> warn, never fail — that file is the user's. Reached only when the
/// plugin declares guidance.
fn instructions_check(base: &Path, plugin: &Plugin) -> DoctorCheck {
    const NAME: &str = "always-apply rule present";
    let path = instructions_path(base, plugin);
    match probe_instructions(base, plugin) {
        Ok(Some(BackendState::Healthy)) => {
            DoctorCheck { name: NAME, status: CheckStatus::Ok(format!("rule present at {}", path.display())) }
        }
        Ok(Some(_)) => DoctorCheck {
            name: NAME,
            status: CheckStatus::Fail {
                problem: format!("our rule missing or drifted at {}", path.display()),
                fix: "run the host's `setup`".into(),
            },
        },
        Ok(None) => DoctorCheck {
            name: NAME,
            status: CheckStatus::Warn(format!("{} holds a foreign (untagged) rule; ours is not written", path.display())),
        },
        Err(e) => DoctorCheck { name: NAME, status: CheckStatus::Warn(e.to_string()) },
    }
}

#[cfg(test)]
#[path = "../../tests/unit/cursor.rs"]
mod cursor_tests;
