//! The kimi-code backend: a full translate into kimi's own config, rooted at
//! `~/.kimi-code/` (override `KIMI_CODE_HOME`). MCP goes through the shared json
//! renderer (`mcp.json` `mcpServers`, Plain shape — kimi's stdio body is
//! `{command,args,env}`); hooks land in `config.toml` as a `[[hooks]]`
//! array-of-tables under CC's exact event names (kimi mirrors them 1:1). The
//! host's always-loaded guidance lands as a tagged block in
//! `~/.kimi-code/AGENTS.md`, marker-id-scoped so the user's lines and a rival
//! plugin's block survive byte-identical. Every mcp
//! key is our own server name and every hook is matched by its command string, so
//! `remove` is exact and a second reconcile is a true `NoOp`. Agent defs land as
//! kimi 2.3.3 user agent profiles in `~/.kimi-code/agents/`, plugin-prefixed kebab
//! files in a dir shared with the user's own; commands have no user-level file
//! surface here and are skipped. Skills land as bare
//! `~/.kimi-code/skills/<name>/SKILL.md`, tagged for ownership.
//!
//! Two products share the `kimi` binary — the legacy python `kimi-cli` (`~/.kimi`)
//! and the current TypeScript `kimi-code` (`~/.kimi-code`). This backend targets the
//! CURRENT one, so it detects on the `~/.kimi-code` config dir (not the ambiguous
//! binary name) and honors `KIMI_CODE_HOME` everywhere.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use super::agentsmd;
use super::cchooks::hook_is_portable;
use super::confedit;
use super::mcpjson::{self, RemoteShape, ServerShape};
use super::report;
use super::skillsdir;
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct KimiBackend;

/// kimi keys remote transport on a `transport` field; `type` is stripped by the
/// non-strict schema and a bare `{url}` infers http, so the majority shape would
/// silently downgrade sse to http.
const SHAPE: ServerShape = ServerShape::plain().with_remote(RemoteShape::TransportKeyed);

impl AgentBackend for KimiBackend {
    fn id(&self) -> &'static str {
        "kimi"
    }

    fn detect(&self) -> bool {
        // The `kimi` binary is shared with the legacy python `kimi-cli`, so it is not
        // a reliable signal for *this* product; the `~/.kimi-code` config dir is the
        // only discriminator. HOME/`KIMI_CODE_HOME`-based, so a test redirecting them
        // also redirects detection.
        kimi_home_opt().is_some_and(|d| d.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // No user-level command file surface exists (kimi's slash-command analog is
        // a `SKILL.md` subdir, not a flat `.md`); mcp + hooks + agent profiles +
        // skills + instructions translate.
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: true,
            commands: false,
            agents: true,
            skills: true,
            instructions: true,
            scopes: &["user"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Compose the two surfaces (mcp.json + the `[[hooks]]` array in config.toml),
        // so a stripped hook table behind a healthy mcp.json reads NeedsRepair.
        // `source` is the one self_heal resolved for this agent (rehydrated `--path`,
        // else the compile-time default), so probe and reconcile render identical bytes.
        let comp = plugin.components(source)?.with_client(self.id());
        let base = kimi_base(scope)?;
        let mcp = mcpjson::probe_surface(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, SHAPE)?;
        let hooks = probe_hooks(&base.join("config.toml"), &comp.hooks)?;
        let skills = skillsdir::probe(&base.join("skills"), plugin, &comp.skills)?;
        // Agent profiles share `agents/` with the user's own; probe compares the
        // same expected set reconcile writes, so a deleted profile is never Healthy.
        let agents = report::probe_files(&expected_agents(&base.join("agents"), plugin.name, &comp.agents), |_, _| true)?;
        let instructions = match agents_md(scope)? {
            Some(path) => agentsmd::probe(&path, plugin)?,
            None => None,
        };
        Ok(report::compose([mcp, hooks, skills, agents, instructions].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?.with_client(self.id());
        let base = kimi_base(scope)?;

        let mut changed = false;
        changed |= mcpjson::reconcile(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, SHAPE)? != Outcome::NoOp;
        changed |= reconcile_hooks(&base.join("config.toml"), &comp.hooks)?;
        changed |= skillsdir::reconcile(&base.join("skills"), plugin, &comp.skills)?;
        changed |= reconcile_agents(&base.join("agents"), plugin.name, &comp.agents)?;
        if let Some(path) = agents_md(scope)? {
            changed |= agentsmd::reconcile(&path, plugin)?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
        let comp = plugin.components(source)?.with_client(self.id());
        let base = kimi_base(scope)?;

        let mut changed = false;
        changed |= mcpjson::remove(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, SHAPE)? != Outcome::NoOp;
        changed |= remove_hooks(&base.join("config.toml"), &comp.hooks)?;
        changed |= skillsdir::remove(&base.join("skills"), plugin, &comp.skills)?;
        changed |= remove_agents(&base.join("agents"), plugin.name, &comp.agents)?;
        if let Some(path) = agents_md(scope)? {
            changed |= agentsmd::remove(&path, plugin)?;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The kimi-code home dir, honoring `KIMI_CODE_HOME` (its documented override) then
/// `~/.kimi-code`. `_opt` never errors so `detect` can call it; a HOME-based fallback
/// means a test redirecting `HOME`/`KIMI_CODE_HOME` also redirects the backend.
fn kimi_home_opt() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("KIMI_CODE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir().map(|h| h.join(".kimi-code"))
}

fn kimi_home() -> Result<PathBuf> {
    kimi_home_opt().ok_or_else(|| Error::Tree("no home directory (HOME and KIMI_CODE_HOME unset); cannot locate ~/.kimi-code".into()))
}

/// The config base for a scope: `~/.kimi-code` (user) or `<cwd>/.kimi-code`
/// (project). Only user scope is a capability (project hooks are undocumented); the
/// project arm is a defensive fallback for kimi's documented project `.kimi-code/`.
fn kimi_base(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => kimi_home(),
        Scope::Project { path } => Ok(path.join(".kimi-code")),
    }
}

/// The user-scope `~/.kimi-code/AGENTS.md` carrying the host's always-loaded
/// guidance. User scope is kimi's only capability, so the project arm is the same
/// defensive fallback shape as [`kimi_base`] and writes nothing.
fn agents_md(scope: &Scope) -> Result<Option<PathBuf>> {
    match scope {
        Scope::User => Ok(Some(kimi_home()?.join("AGENTS.md"))),
        Scope::Project { .. } => Ok(None),
    }
}

// --- hooks -------------------------------------------------------------------

/// Kimi's hook event names match Claude Code's 1:1 for the events both define, so a
/// supported CC event passes through unchanged; a CC event kimi has no name for is
/// skipped rather than written under a guessed name. Kimi's extra events
/// (`PostToolUseFailure`, `PermissionResult`, `Interrupt`, ...) have no CC source.
const KIMI_EVENTS: &[&str] = &[
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "StopFailure",
    "PermissionRequest",
    "PermissionResult",
    "SessionStart",
    "SessionEnd",
    "SubagentStart",
    "SubagentStop",
    "Interrupt",
    "PreCompact",
    "PostCompact",
    "Notification",
];

fn map_event(cc_event: &str) -> Option<&'static str> {
    KIMI_EVENTS.iter().copied().find(|e| *e == cc_event)
}

/// One `[[hooks]]` table: `event` + optional `matcher` + `command`. Deterministic, so
/// a re-reconcile is byte-identical. `timeout` is left unset (kimi defaults to 30s).
fn render_hook_table(event: &str, hook: &HookBinding) -> Table {
    let mut t = Table::new();
    t.insert("event", value(event));
    if let Some(matcher) = &hook.matcher {
        t.insert("matcher", value(matcher.as_str()));
    }
    t.insert("command", value(hook.command.as_str()));
    t
}

/// The `[[hooks]]` array-of-tables, created if absent. `None` only when the user's
/// `hooks` key exists as some other type (e.g. an inline array) — we never clobber it.
fn hooks_array(doc: &mut DocumentMut) -> Option<&mut ArrayOfTables> {
    doc.as_table_mut().entry("hooks").or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new())).as_array_of_tables_mut()
}

/// Whether a `[[hooks]]` entry deep-equal to ours already exists (event + matcher +
/// command), so a second reconcile re-adds nothing and stays a true `NoOp`.
fn hook_present(arr: &ArrayOfTables, event: &str, hook: &HookBinding) -> bool {
    arr.iter().any(|t| {
        t.get("event").and_then(Item::as_str) == Some(event)
            && t.get("command").and_then(Item::as_str) == Some(hook.command.as_str())
            && t.get("matcher").and_then(Item::as_str) == hook.matcher.as_deref()
    })
}

/// Append-if-absent our hook tables to `config.toml`'s `[[hooks]]`, leaving the
/// user's own entries. Non-portable hooks (`hook_is_portable`) and events with no
/// kimi analog (`map_event` -> `None`) are skipped, same as mcp servers. Skips
/// `toml_edit` entirely when nothing survives, so no empty `hooks` array is written.
fn reconcile_hooks(config: &Path, hooks: &[HookBinding]) -> Result<bool> {
    let writable: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|e| (e, h))).collect();
    if writable.is_empty() {
        return Ok(false);
    }
    confedit::toml_edit(config, |doc| {
        let Some(arr) = hooks_array(doc) else {
            return Ok(()); // user's `hooks` is a non-array-of-tables; never clobber it
        };
        for (event, hook) in &writable {
            if !hook_present(arr, event, hook) {
                arr.push(render_hook_table(event, hook));
            }
        }
        Ok(())
    })
}

/// Classify the `[[hooks]]` surface for `probe`, mirroring `reconcile_hooks`'s
/// writable filter (portable AND a mapped kimi event) and its `hook_present`
/// deep-equal check. `None` when nothing is writable; a missing config.toml where we
/// would write is `Absent`; all our tables present -> `Healthy`; some -> `NeedsRepair`.
fn probe_hooks(config: &Path, hooks: &[HookBinding]) -> Result<Option<BackendState>> {
    let writable: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|e| (e, h))).collect();
    if writable.is_empty() {
        return Ok(None);
    }
    let text = match fs::read_to_string(config) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some(BackendState::Absent)),
        Err(source) => return Err(Error::Io { context: format!("reading {}", config.display()), source }),
    };
    let doc: DocumentMut =
        text.parse().map_err(|e: toml_edit::TomlError| Error::Config { path: config.display().to_string(), detail: e.to_string() })?;
    let arr = doc.get("hooks").and_then(Item::as_array_of_tables);
    let present = writable.iter().filter(|(event, hook)| arr.is_some_and(|a| hook_present(a, event, hook))).count();
    Ok(Some(if present == 0 {
        BackendState::Absent
    } else if present == writable.len() {
        BackendState::Healthy
    } else {
        BackendState::NeedsRepair
    }))
}

/// Strip exactly our `[[hooks]]` entries (matched by command string), leaving the
/// user's — including one under an event we also write to. A hook we never wrote (a
/// non-portable one, or a user's own) survives, since it never enters `ours`. The
/// array goes with them once ours were the last entries in it — the exact inverse of
/// the [`hooks_array`] that created it — and a `config.toml` left holding nothing
/// goes too. An emptied array-of-tables renders to zero bytes while still keying the
/// root, so pruning it is what lets the file arm see an empty document.
fn remove_hooks(config: &Path, hooks: &[HookBinding]) -> Result<bool> {
    if !config.exists() {
        return Ok(false);
    }
    let ours: BTreeSet<&str> = hooks.iter().filter(|h| hook_is_portable(h)).map(|h| h.command.as_str()).collect();
    confedit::toml_remove(config, |doc| {
        confedit::toml_prune(doc, "hooks", |item| {
            if let Some(arr) = item.as_array_of_tables_mut() {
                arr.retain(|t| t.get("command").and_then(Item::as_str).is_none_or(|c| !ours.contains(c)));
            }
            Ok(())
        })
    })
}

// --- agents ------------------------------------------------------------------

/// Render a CC agent def as a kimi 2.3.3 user agent profile, or `None` when kimi
/// could not load the result (an un-sluggable name, a missing/blank `description`,
/// or an empty body) — such a file would be silently skipped by kimi's scanner on
/// every scan. kimi's `parseAgentFileText` enforces `^[a-z0-9]+(-[a-z0-9]+)*$` on
/// the frontmatter `name`, so the name source (frontmatter `name` else the flat
/// stem) is slugged, plugin-prefixed, and the whole composed name slugged again —
/// a plugin name is only a single file-name component, not kebab. The file stem IS
/// that composed kebab name: kimi's directory walk skips every dot-leading entry,
/// so a raw plugin name could hide the profile from the scan while every check
/// stays green. `tools`/`disallowedTools` copy through as their comma strings;
/// there is no agent `model` field, so the CC model alias is dropped. The body (the
/// prompt) copies through trimmed with one trailing newline. Deterministic, so a
/// re-reconcile is byte-identical.
fn rendered_agent(plugin: &str, doc: &MarkdownDoc) -> Option<(String, String)> {
    let name = doc.frontmatter.get("name").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| doc.flat_stem());
    let slug = slugify(&name);
    if slug.is_empty() {
        return None;
    }
    let description = doc.frontmatter.get("description").and_then(Value::as_str).map(str::trim).filter(|d| !d.is_empty())?;
    let body = doc.body.trim();
    if body.is_empty() {
        return None;
    }

    let kimi_name = slugify(&format!("{plugin}-{slug}"));
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {}\n", confedit::yaml_scalar(&kimi_name)));
    out.push_str(&format!("description: {}\n", confedit::yaml_scalar(description)));
    for key in ["tools", "disallowedTools"] {
        if let Some(tools) = doc.frontmatter.get(key).and_then(Value::as_str) {
            out.push_str(&format!("{key}: {}\n", confedit::yaml_scalar(tools)));
        }
    }
    out.push_str("---\n\n");
    out.push_str(body);
    out.push('\n');
    Some((format!("{kimi_name}.md"), out))
}

/// Kimi's name pattern is `^[a-z0-9]+(-[a-z0-9]+)*$`: lowercase, collapse every run
/// outside `[a-z0-9]` to one `-`, trim the ends. An input holding no alphanumerics
/// yields an empty slug.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// The `(path, rendered bytes)` agent profiles probe compares against disk, keyed
/// off the same [`rendered_agent`] reconcile writes — a doc kimi could not load
/// renders no file and enters none of the probe/reconcile/remove sets. Two docs
/// that slugify to one profile name collide (kimi keys profiles on that name, not
/// the filename), so the docs are sorted by `rel` and the first per path wins: one
/// path, one byte set, a converged reconcile is a true `NoOp` instead of churn.
fn expected_agents(agent_root: &Path, plugin: &str, agents: &[MarkdownDoc]) -> Vec<(PathBuf, Vec<u8>)> {
    let mut docs: Vec<&MarkdownDoc> = agents.iter().collect();
    docs.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for doc in docs {
        let Some((file, bytes)) = rendered_agent(plugin, doc) else {
            continue;
        };
        if !seen.insert(file.clone()) {
            continue;
        }
        out.push((agent_root.join(file), bytes.into_bytes()));
    }
    out
}

/// Write our agent profiles, `write_file_idem` per file so a converged reconcile
/// returns false. Files are plugin-prefixed in a dir shared with the user's own
/// profiles, so nothing else there is ever written.
fn reconcile_agents(agent_root: &Path, plugin: &str, agents: &[MarkdownDoc]) -> Result<bool> {
    let mut changed = false;
    for (path, bytes) in expected_agents(agent_root, plugin, agents) {
        changed |= confedit::write_file_idem(&path, &bytes)?;
    }
    Ok(changed)
}

/// Delete exactly the profiles [`reconcile_agents`] wrote, `remove_file_idem` per
/// exact path — a doc that rendered no file is no removal candidate either, and a
/// user's own profile in the shared dir is never in the set.
fn remove_agents(agent_root: &Path, plugin: &str, agents: &[MarkdownDoc]) -> Result<bool> {
    let mut changed = false;
    for (path, _) in expected_agents(agent_root, plugin, agents) {
        changed |= confedit::remove_file_idem(&path)?;
    }
    Ok(changed)
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &KimiBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "kimi detected", status: CheckStatus::Ok("~/.kimi-code present".into()) }
    } else {
        DoctorCheck {
            name: "kimi detected",
            status: CheckStatus::Fail {
                problem: "kimi-code not detected (~/.kimi-code absent)".into(),
                fix: "install it with `npm install -g @moonshot-ai/kimi-code`, then run `kimi` once".into(),
            },
        }
    });

    let base = match kimi_base(&Scope::User) {
        Ok(base) => base,
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp config file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };
    let mcp = base.join("mcp.json");

    let root = report::read_json_config(&mut checks, "mcp config file", &mcp);

    let Some(comp) = report::components(&mut checks, plugin, source).map(|c| c.with_client(backend.id())) else {
        return checks;
    };

    let writable = mcpjson::writable_names(&comp.mcp_servers, SHAPE);
    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        &writable,
        root.as_ref(),
        &["mcpServers"],
        "not in mcp.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    checks.push(check_hooks_present(&comp.hooks, &base.join("config.toml")));
    checks.push(check_agents_present(&comp.agents, &base.join("agents"), plugin.name));

    if plugin.instructions.is_some() {
        checks.push(agentsmd::guidance_check(agents_md(&Scope::User), plugin));
    }

    checks
}

/// Unlike codex, kimi hooks fire the moment they are written (no `/hooks` trust
/// gate), so a present hook is a plain `Ok`; a missing one we should have written is
/// a `Fail`. Substring scan of `config.toml` — reconcile already refused an
/// unparseable file, so a raw text check is enough to confirm presence.
fn check_hooks_present(hooks: &[HookBinding], config: &Path) -> DoctorCheck {
    let name = "translated hooks present";
    let skipped = report::skipped_hooks(hooks);
    let ours: Vec<&str> =
        hooks.iter().filter(|h| hook_is_portable(h) && map_event(&h.event).is_some()).map(|h| h.command.as_str()).collect();
    if ours.is_empty() {
        return report::note_skipped(DoctorCheck { name, status: CheckStatus::Ok("no hooks to translate".into()) }, &skipped);
    }
    let text = fs::read_to_string(config).unwrap_or_default();
    let missing: Vec<&str> = ours.iter().copied().filter(|c| !text.contains(c)).collect();
    let check = if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} hook(s) present in config.toml", ours.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("hook(s) missing from config.toml: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    };
    report::note_skipped(check, &skipped)
}

/// A plugin shipping no agents has nothing to translate; otherwise every rendered
/// profile (see [`expected_agents`]) must exist on disk — a doc that rendered no
/// file is by design, never a missing one. A wholly untranslatable set (every doc
/// skipped) is a translate limit reported as an `Ok` naming the skip count, never a
/// cheerful `0 agent file(s) present`. Two docs slugifying to one profile name are
/// a `Fail` naming both, never healthy: one file on disk cannot be "present" for
/// both of them.
fn check_agents_present(agents: &[MarkdownDoc], agent_root: &Path, plugin: &str) -> DoctorCheck {
    let name = "translated agents present";
    if agents.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no agents to translate".into()) };
    }
    let mut docs: Vec<&MarkdownDoc> = agents.iter().collect();
    docs.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut by_file: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for doc in &docs {
        if let Some((file, _)) = rendered_agent(plugin, doc) {
            by_file.entry(file).or_default().push(&doc.rel);
        }
    }
    if by_file.is_empty() {
        return DoctorCheck {
            name,
            status: CheckStatus::Ok(format!(
                "no translatable agent profile: {} agent file(s) skipped (kimi needs a description and a prompt body)",
                agents.len()
            )),
        };
    }
    if let Some((file, rels)) = by_file.iter().find(|(_, rels)| rels.len() > 1) {
        return DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("agent profile name collision: {} render to the same profile `{file}`", rels.join(" and ")),
                fix: "rename one of the colliding agent files".into(),
            },
        };
    }
    let expected = expected_agents(agent_root, plugin, agents);
    let missing: Vec<String> =
        expected.iter().map(|(path, _)| path).filter(|path| !path.exists()).map(|path| path.display().to_string()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} agent file(s) present", expected.len())) }
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
#[path = "../../tests/unit/kimi.rs"]
mod kimi_tests;
