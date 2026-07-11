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
//! See `docs/harness/droid.md` for the full mapping + skipped surfaces.

use std::collections::BTreeSet;
use std::fmt::Write as _;
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

pub(crate) struct DroidBackend;

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
        Capabilities { plugins: false, mcp: true, hooks: true, scopes: &["user", "project"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        // Ownership is defined by our mcp server keys (the canonical "are we here"
        // signal); the shared probe returns Healthy — never Absent — for an mcp-less
        // plugin, so a present marker is never dropped. Source::Embedded is the only
        // steady-state source for a non-CC backend (github unsupported, path is
        // install-only), mirroring the claude probe keying on compile-time metadata.
        let comp = plugin.components(&Source::Embedded)?;
        let mcp = factory_dir(scope)?.join("mcp.json");
        mcpjson::probe(&mcp, &["mcpServers"], &comp.mcp_servers, ServerShape::Plain)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let base = factory_dir(scope)?;

        let mut changed = false;
        changed |= mcpjson::reconcile(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, ServerShape::Plain)? != Outcome::NoOp;
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
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let base = factory_dir(scope)?;

        let mut changed = false;
        changed |= mcpjson::remove(&base.join("mcp.json"), &["mcpServers"], &portable_names(&comp.mcp_servers))? != Outcome::NoOp;
        changed |= remove_hooks(&base.join("hooks.json"), &comp.hooks)?;

        // We wrote each doc as one flat, plugin-prefixed file, so removing exactly those
        // paths never reaches a user's own command/droid or a droid built-in.
        for (subdir, prefix, docs) in [("commands", "commands/", &comp.commands), ("droids", "agents/", &comp.agents)] {
            for doc in docs {
                let path = base.join(subdir).join(doc_filename(plugin.name, &doc.rel, prefix));
                changed |= remove_file_idem(&path)?;
            }
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
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

/// Server names `reconcile` actually writes (the shared renderer skips non-portable
/// ones). `remove` keys off the same set so an unfiltered name list can never delete
/// a user server sharing a name with one we declared but never wrote.
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
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

/// A `${CLAUDE_PLUGIN_ROOT}` reference only expands inside Claude Code's own hook
/// runner; droid expands actual shell env vars but never sets that token, so such a
/// command would spawn the literal, unexpanded string. Mirrors `McpServer::is_portable`
/// (applied locally: `HookBinding` has no such method in the shared components IR).
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
    json_edit(hooks_path, |root| {
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

/// A YAML scalar: bare when it cannot be misparsed as a flow/indicator token, else a
/// double-quoted string with the minimal escapes.
fn yaml_scalar(s: &str) -> String {
    let needs_quote = s.is_empty()
        || s.starts_with(|c: char| c.is_ascii_whitespace())
        || s.ends_with(|c: char| c.is_ascii_whitespace())
        || s.contains(['"', '\\', '\n', '\r', '\t', ':', '#', '[', ']', '{', '}', ',', '&', '*', '!', '|', '>', '\'', '%', '@', '`'])
        || matches!(s.to_ascii_lowercase().as_str(), "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~");
    if !needs_quote {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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

    let root = match fs::read(&mcp) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "mcp config file", status: CheckStatus::Ok(format!("{} parses", mcp.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "mcp config file",
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
                name: "mcp config file",
                status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", mcp.display())),
            });
            None
        }
        Err(e) => {
            checks
                .push(DoctorCheck { name: "mcp config file", status: CheckStatus::Warn(format!("could not read {}: {e}", mcp.display())) });
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
    checks.push(check_docs_present("commands", "commands/", &comp.commands, plugin.name, &base));
    checks.push(check_docs_present("droids", "agents/", &comp.agents, plugin.name, &base));

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
