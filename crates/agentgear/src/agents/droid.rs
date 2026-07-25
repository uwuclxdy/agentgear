//! The Factory `droid` backend: a full translate into droid's own config tree under
//! `~/.factory` (user) or `<cwd>/.factory` (project). MCP goes through the shared
//! json renderer (Plain `{command,args,env}`) into the `mcpServers` key of a
//! dedicated `mcp.json`; CC hooks land in a dedicated `hooks.json` under the CC-shape
//! `{"hooks": {Event: [...]}}` wrapper (droid's event names match CC 1:1, so the map
//! is identity for all nine). CC commands become droid custom slash-commands (verbatim
//! markdown, since droid's command format is the CC format) and CC agents become droid
//! **custom droids** (`droids/<name>.md`, markdown + YAML frontmatter with a namespaced
//! `name`).
//!
//! droid loads only top-level files from `commands/`/`droids/` (nested dirs are
//! ignored), so a translated doc is written flat as `<plugin>-<stem>.md` rather than
//! under a plugin subdir the way the gemini/devin backends can. The prefix keeps every
//! file identifiable as ours: `remove` deletes exactly those files, mcp is keyed by our
//! server names, hooks by our command strings — so a second reconcile is a true `NoOp`.
//! Skills land as bare `<base>/skills/<name>/SKILL.md` (droid's first-class skill
//! surface, both scopes), tagged for ownership. See `docs/harness/droid.md` for the
//! full mapping.
//!
//! One surface is not a translation of anything in the plugin tree: the host-owned
//! status line, in `<base>/settings.json` at the ROOT `statusLine` key. It runs the
//! shared [`super::statuslinejson`] slot lifecycle on both scopes.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::cchooks::{hook_is_portable, render_hook_group};
use super::confedit::{json_edit, json_obj_at, json_prune_obj, json_remove, remove_file_idem, write_file_idem, yaml_scalar};
use super::mcpjson::{self, ServerShape};
use super::report;
use super::skillsdir;
use super::statuslinejson::{self, SlotShape};
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct DroidBackend;

/// Root-level `statusLine` in `<base>/settings.json`. NOT nested under `general`:
/// droid's accessor reads `settings.general?.statusLine` only because a flat file is
/// wrapped under `general` at load time — on disk the key is root-level, which is what
/// Factory's own `/statusline` setup agent writes.
const STATUSLINE_SLOT: &[&str] = &["statusLine"];

/// Rows droid may render. `compose` structurally emits at least TWO rows whenever the
/// user already had a status line (ours, then theirs), and droid's `maxRows` defaults
/// to 1 — so leaving it unset would silently clip off exactly the row the whole compose
/// design exists to preserve. Its zod schema is `int 1..=3` and the renderer clamps with
/// `Math.min(3, Math.max(1, floor(maxRows ?? 1)))`, so it reads as a cap rather than a
/// reservation; 3 is the ceiling and costs nothing when only one row is printed.
/// UNPROVEN at render time — taken from the schema and the clamp, not from an observed
/// render (`docs/research/statusline-survey.md` §3).
const STATUSLINE_MAX_ROWS: u8 = 3;

/// CC's body plus droid's own row cap. `type` is OPTIONAL here and accepted — the
/// earlier "no `type`, and the absence is load-bearing" reading is refuted by the zod
/// schema (`type: literal("command").optional()`) and by Factory's own agent emitting
/// it — so droid reuses `TypedCommand` rather than earning a variant of its own.
const STATUSLINE_SHAPE: SlotShape = SlotShape::typed_command().with_max_rows(STATUSLINE_MAX_ROWS);

/// The settings file the slot lives in, or `None` when the host declares no status
/// line. Both scopes are real, so this resolves through the backend's own existing
/// config-base resolver rather than a second copy of it.
fn statusline_target(plugin: &Plugin, scope: &Scope) -> Result<Option<PathBuf>> {
    statuslinejson::target(plugin, DroidBackend.id(), STATUSLINE_SHAPE, || Ok(factory_dir(scope)?.join("settings.json")))
}

impl AgentBackend for DroidBackend {
    fn id(&self) -> &'static str {
        "droid"
    }

    fn detect(&self) -> bool {
        // `~/.factory` is HOME-based (not XDG), so a test redirecting `HOME` also
        // redirects detection; droid has no user-config-dir override env. FACTORY_API_KEY
        // is deliberately NOT a detection signal — the brief flags it as an input auth
        // var (set for CI even where droid isn't installed), not a session-set marker.
        which::which("droid").is_ok() || dirs::home_dir().is_some_and(|h| h.join(".factory").is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // Full surface: mcp + hooks + commands + agents (custom droids) + skills.
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: true,
            commands: true,
            agents: true,
            skills: true,
            instructions: false,
            statusline: true,
            scopes: &["user", "project"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Compose every surface (mcp.json, hooks.json, command + custom-droid files), so
        // a dropped hook group or missing command/droid behind a healthy mcp.json reads
        // NeedsRepair. `source` is the one self_heal resolved for this agent (rehydrated
        // `--path`, else the compile-time default), so probe and reconcile render
        // identical bytes.
        let comp = plugin.components(source)?.with_client(self.id());
        let base = factory_dir(scope)?;
        let mcp = mcpjson::probe_surface(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, ServerShape::plain())?;
        let hooks = report::probe_json_entries(&base.join("hooks.json"), &hook_entries(&comp.hooks))?;
        let commands = report::probe_files(
            &expected_docs(&base.join("commands"), plugin.name, "commands/", &comp.commands, |doc| doc.raw.clone()),
            |_, _| true,
        )?;
        let droids = report::probe_files(
            &expected_docs(&base.join("droids"), plugin.name, "agents/", &comp.agents, |doc| {
                render_droid(plugin.name, &doc.rel, doc).into_bytes()
            }),
            |_, _| true,
        )?;
        let skills = skillsdir::probe(&base.join("skills"), plugin, &comp.skills)?;
        // The slot cannot carry presence on its own: a foreign line reads `Absent`
        // (`statuslinejson::state`), so a plugin the user removed stays `Absent` here
        // instead of handing self_heal's adopt row a reason to reinstall it.
        let statusline = match statusline_target(plugin, scope)? {
            Some(path) => statuslinejson::state(&path, STATUSLINE_SLOT, plugin, self.id(), STATUSLINE_SHAPE)?,
            None => None,
        };
        Ok(report::compose([mcp, hooks, commands, droids, skills, statusline].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?.with_client(self.id());
        let base = factory_dir(scope)?;

        let mut changed = false;
        changed |= mcpjson::reconcile(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, ServerShape::plain())? != Outcome::NoOp;
        changed |= reconcile_hooks(&base.join("hooks.json"), &comp.hooks)?;

        // Commands are droid's own format already (markdown + frontmatter + $ARGUMENTS),
        // so a verbatim copy is the faithful translation; the name comes from the flat
        // filename, not frontmatter. Custom droids need a namespaced `name` key.
        for doc in &comp.commands {
            let path = base.join("commands").join(doc_filename(plugin.name, &doc.rel, "commands/"));
            changed |= write_file_idem(&path, &doc.raw)?;
        }
        for doc in &comp.agents {
            let path = base.join("droids").join(doc_filename(plugin.name, &doc.rel, "agents/"));
            changed |= write_file_idem(&path, render_droid(plugin.name, &doc.rel, doc).as_bytes())?;
        }
        changed |= skillsdir::reconcile(&base.join("skills"), plugin, &comp.skills)?;
        if let Some(path) = statusline_target(plugin, scope)? {
            changed |= statuslinejson::reconcile(&path, STATUSLINE_SLOT, plugin, &desired.source, scope, self.id(), STATUSLINE_SHAPE)?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
        let comp = plugin.components(source)?.with_client(self.id());
        let base = factory_dir(scope)?;

        let mut changed = false;
        changed |= mcpjson::remove(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, ServerShape::plain())? != Outcome::NoOp;
        changed |= remove_hooks(&base.join("hooks.json"), &comp.hooks)?;

        // We wrote each doc as one flat, plugin-prefixed file, so removing exactly those
        // paths never reaches a user's own command/droid or a droid built-in.
        for (subdir, prefix, docs) in [("commands", "commands/", &comp.commands), ("droids", "agents/", &comp.agents)] {
            for doc in docs {
                let path = base.join(subdir).join(doc_filename(plugin.name, &doc.rel, prefix));
                changed |= remove_file_idem(&path)?;
            }
        }
        changed |= skillsdir::remove(&base.join("skills"), plugin, &comp.skills)?;
        // Exact-remove for a slot means RESTORE: put back what our write displaced.
        if let Some(path) = statusline_target(plugin, scope)? {
            changed |= statuslinejson::remove(&path, STATUSLINE_SLOT, plugin, scope, self.id(), STATUSLINE_SHAPE)?;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    /// `settings.json` is a file the USER owns and it outlives droid's config tree and
    /// the `droid` binary, so the slot goes back on every teardown branch that reaches
    /// the marker clear — the skips included, since the marker holds the only copy of
    /// what we displaced.
    fn forget(&self, plugin: &Plugin, scope: &Scope) -> Result<()> {
        let Some(path) = statusline_target(plugin, scope)? else {
            return Ok(());
        };
        statuslinejson::remove(&path, STATUSLINE_SLOT, plugin, scope, self.id(), STATUSLINE_SHAPE).map(|_| ())
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The `.factory` config base for a scope: `~/.factory` (user) or `<cwd>/.factory`
/// (project). User scope needs `HOME`; a missing home is a clear, actionable error
/// rather than a silent write to the wrong place.
fn factory_dir(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => dirs::home_dir()
            .map(|h| h.join(".factory"))
            .ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.factory".into())),
        Scope::Project { path } => Ok(path.join(".factory")),
    }
}

/// `commands/hello.md` -> `<plugin>-hello`; a nested path flattens (`a/b.md` ->
/// `<plugin>-a-b`) because droid only loads top-level files. The plugin prefix keeps
/// the file identifiable as ours for an exact `remove`.
fn namespaced(plugin: &str, rel: &str, prefix: &str) -> String {
    let stripped = rel.strip_prefix(prefix).unwrap_or(rel);
    let stem = stripped.strip_suffix(".md").unwrap_or(stripped);
    format!("{plugin}-{}", stem.replace(['/', '\\'], "-"))
}

/// The flat `<plugin>-<stem>.md` filename a translated command/droid is written as.
fn doc_filename(plugin: &str, rel: &str, prefix: &str) -> String {
    format!("{}.md", namespaced(plugin, rel, prefix))
}

// --- hooks -------------------------------------------------------------------

/// Map a CC hook event to droid's. droid hosts the full CC event set under the same
/// names, so the map is identity for all nine; an unknown/future event maps to `None`
/// and is skipped rather than written under a guessed name.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "PreToolUse" => Some("PreToolUse"),
        "PostToolUse" => Some("PostToolUse"),
        "UserPromptSubmit" => Some("UserPromptSubmit"),
        "Notification" => Some("Notification"),
        "Stop" => Some("Stop"),
        "SubagentStop" => Some("SubagentStop"),
        "PreCompact" => Some("PreCompact"),
        "SessionStart" => Some("SessionStart"),
        "SessionEnd" => Some("SessionEnd"),
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

/// The `(path, rendered bytes)` files `probe` compares against disk for a surface
/// dir, keyed off the same `doc_filename` + render `reconcile` writes.
fn expected_docs(
    dir: &Path, plugin: &str, prefix: &str, docs: &[MarkdownDoc], render: impl Fn(&MarkdownDoc) -> Vec<u8>,
) -> Vec<(PathBuf, Vec<u8>)> {
    docs.iter().map(|doc| (dir.join(doc_filename(plugin, &doc.rel, prefix)), render(doc))).collect()
}

/// Add-if-absent our hook groups under each mapped event in the `hooks.json` wrapper,
/// leaving the user's own groups in place. Idempotent: a group already present
/// (deep-equal) is not re-added. Non-portable hooks and events with no droid analog are
/// skipped, same as mcp servers. Skips the whole edit when nothing is writable so no
/// empty `"hooks": {}` key is created for zero writes.
fn reconcile_hooks(hooks_path: &Path, hooks: &[HookBinding]) -> Result<bool> {
    let writable: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|event| (event, h))).collect();
    if writable.is_empty() {
        return Ok(false);
    }
    json_edit(hooks_path, |root| {
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

/// Strip exactly our hook handlers (matched by command string) from every event in the
/// `hooks` wrapper, dropping a group or event array we emptied. A user handler sharing a
/// group with ours (or a group of their own) survives. The ownership set mirrors
/// `reconcile_hooks`'s writable filter (portable AND mapped) so a command from an
/// unmapped event — never written here — is never treated as ours to remove.
fn remove_hooks(hooks_path: &Path, hooks: &[HookBinding]) -> Result<bool> {
    if !hooks_path.exists() {
        return Ok(false);
    }
    let ours: BTreeSet<&str> =
        hooks.iter().filter(|h| hook_is_portable(h) && map_event(&h.event).is_some()).map(|h| h.command.as_str()).collect();
    json_remove(hooks_path, |root| {
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

// --- custom droids -----------------------------------------------------------

/// Render a droid custom droid (`droids/<name>.md`): a namespaced `name`, the CC agent
/// doc's remaining frontmatter (`description`/`model`/… verbatim), then the body. droid
/// reads YAML frontmatter + a markdown system prompt — the same shape as the CC source —
/// so translation is a re-emit with an ownership-safe, collision-free `name`.
/// Deterministic (BTreeMap frontmatter iteration is sorted) so a re-reconcile is
/// byte-identical.
fn render_droid(plugin: &str, rel: &str, doc: &MarkdownDoc) -> String {
    let mut out = String::from("---\n");
    let _ = writeln!(out, "name: {}", yaml_scalar(&namespaced(plugin, rel, "agents/")));
    for (key, value) in &doc.frontmatter {
        if key == "name" {
            continue; // overridden with the namespaced name above
        }
        let _ = writeln!(out, "{key}: {}", yaml_value(value));
    }
    out.push_str("---\n\n");
    out.push_str(doc.body.trim());
    out.push('\n');
    out
}

fn yaml_value(value: &Value) -> String {
    match value {
        Value::String(s) => yaml_scalar(s),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        // Non-scalar frontmatter is out of the CC agent shape; stringify + quote so it
        // can never break the document.
        other => yaml_scalar(&other.to_string()),
    }
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &DroidBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "droid detected", status: CheckStatus::Ok("`droid` on PATH or ~/.factory present".into()) }
    } else {
        DoctorCheck {
            name: "droid detected",
            status: CheckStatus::Fail {
                problem: "droid CLI not detected".into(),
                fix: "install it with `curl -fsSL https://app.factory.ai/cli | sh`".into(),
            },
        }
    });

    let base = match factory_dir(&Scope::User) {
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

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcpServers"],
        "not in mcp.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    checks.push(check_docs_present("commands", "commands/", &comp.commands, plugin.name, &base));
    checks.push(check_docs_present("droids", "agents/", &comp.agents, plugin.name, &base));
    // Absent entirely for a host that declares no status line.
    checks.extend(statuslinejson::check(Ok(base.join("settings.json")), STATUSLINE_SLOT, plugin, backend.id(), STATUSLINE_SHAPE, "droid"));

    checks
}

fn check_docs_present(subdir: &str, prefix: &str, docs: &[MarkdownDoc], plugin: &str, base: &Path) -> DoctorCheck {
    // A shared name so the doctor fan-out's `<id>: ` prefix stays the whole label.
    let name: &'static str = if subdir == "commands" { "translated commands present" } else { "translated droids present" };
    if docs.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok(format!("no {subdir} to translate")) };
    }
    let missing: Vec<String> =
        docs.iter().map(|d| doc_filename(plugin, &d.rel, prefix)).filter(|f| !base.join(subdir).join(f).exists()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} {subdir} file(s) present", docs.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("{subdir} file(s) missing: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/droid.rs"]
mod droid_tests;
