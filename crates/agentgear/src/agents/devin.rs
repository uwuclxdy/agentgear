//! The Devin Local backend: a full translate into devin's own config tree. Devin
//! reuses the Claude-Code family of shapes, so translation is mostly a re-emit.
//! MCP goes through the shared json renderer (Plain `{command,args,env}`) into the
//! `mcpServers` key of devin's `config.json` (`~/.config/devin/config.json` for
//! user, `<cwd>/.devin/config.json` for project); CC hooks land in that same file
//! under a `hooks` key using the identical CC hook shape (devin's event names —
//! `SessionStart`/`UserPromptSubmit`/… — match CC 1:1). CC commands become devin
//! **skills** (`skills/<name>/SKILL.md`) and CC agents become devin native
//! **subagents** (`agents/<name>/AGENT.md`), both markdown + YAML frontmatter.
//!
//! Ownership: mcp servers are keyed by our server names; skills/agents live in
//! plugin-namespaced dirs (`<plugin>-<stem>/`) that we own whole, so `remove` is
//! exact and a second reconcile is a true `NoOp`. A skill/agent name is prefixed
//! with the plugin so it can never collide with a devin built-in profile
//! (`subagent_explore`/`subagent_general`) or a user's own. See
//! `docs/harness/devin.md` for the full mapping + skipped surfaces (rules/skills).

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::confedit::{json_edit, json_obj_at, write_file_idem};
use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, IoContext, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct DevinBackend;

impl AgentBackend for DevinBackend {
    fn id(&self) -> &'static str {
        "devin"
    }

    fn detect(&self) -> bool {
        // `~/.config/devin` is XDG-based (so a test redirecting `XDG_CONFIG_HOME`
        // redirects detection); a project `.devin/` (or a legacy `.cognition/`
        // symlink from the pre-`2026.3.20-2` layout) counts too, as does the CLI.
        which::which("devin").is_ok() || user_config_base().is_some_and(|b| b.is_dir()) || project_marker_present()
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
        let config = config_base(scope)?.join("config.json");
        mcpjson::probe(&config, &["mcpServers"], &comp.mcp_servers, ServerShape::Plain)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let base = config_base(scope)?;
        let config = base.join("config.json");

        let mut changed = false;
        changed |= mcpjson::reconcile(&config, &["mcpServers"], &comp.mcp_servers, ServerShape::Plain)? != Outcome::NoOp;
        changed |= reconcile_hooks(&config, &comp.hooks)?;

        // Each skill/agent is its own depth-1 dir (devin discovers `skills/<name>/
        // SKILL.md` and `agents/<name>/AGENT.md`), plugin-prefixed so it stays ours.
        for doc in &comp.commands {
            let path = base.join("skills").join(namespaced(plugin.name, &doc.rel, "commands/")).join("SKILL.md");
            changed |= write_file_idem(&path, render_doc(plugin.name, &doc.rel, "commands/", doc).as_bytes())?;
        }
        for doc in &comp.agents {
            let path = base.join("agents").join(namespaced(plugin.name, &doc.rel, "agents/")).join("AGENT.md");
            changed |= write_file_idem(&path, render_doc(plugin.name, &doc.rel, "agents/", doc).as_bytes())?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let base = config_base(scope)?;
        let config = base.join("config.json");

        let mut changed = false;
        changed |= mcpjson::remove(&config, &["mcpServers"], &portable_names(&comp.mcp_servers))? != Outcome::NoOp;
        changed |= remove_hooks(&config, &comp.hooks)?;

        // We own each `<plugin>-<stem>/` dir whole, so a recursive drop is exact and
        // never reaches a devin built-in profile or a user's own skill/agent.
        for (subdir, prefix, docs) in [("skills", "commands/", &comp.commands), ("agents", "agents/", &comp.agents)] {
            for doc in docs {
                let dir = base.join(subdir).join(namespaced(plugin.name, &doc.rel, prefix));
                if dir.exists() {
                    fs::remove_dir_all(&dir).io_ctx(|| format!("removing {}", dir.display()))?;
                    changed = true;
                }
            }
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The `~/.config/devin` user base. `dirs::config_dir()` honors `XDG_CONFIG_HOME`
/// (Linux) and is `%APPDATA%` (Windows) — both matching devin. On macOS devin uses
/// `~/.config/devin` too (documented in `docs/harness/devin.md`), unlike
/// `dirs::config_dir()`'s platform default of `~/Library/Application Support`; the
/// macOS arm below replicates dirs' own XDG-or-home-fallback logic instead of that
/// platform default so the path matches what devin actually reads.
fn user_config_base() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .map(|c| c.join("devin"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        dirs::config_dir().map(|c| c.join("devin"))
    }
}

/// The devin config base for a scope: `~/.config/devin` (user) or `<cwd>/.devin`
/// (project). User scope needs a config dir; a missing one is a clear, actionable
/// error rather than a silent write to the wrong place.
fn config_base(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => user_config_base()
            .ok_or_else(|| Error::Tree("no config directory (XDG_CONFIG_HOME and HOME both unset); cannot locate ~/.config/devin".into())),
        Scope::Project { path } => Ok(path.join(".devin")),
    }
}

/// A project `.devin/` (or the legacy `.cognition/` symlink) under the current
/// directory means devin is configured for this checkout.
fn project_marker_present() -> bool {
    std::env::current_dir().is_ok_and(|d| d.join(".devin").is_dir() || d.join(".cognition").is_dir())
}

/// Server names `reconcile` actually writes (the shared renderer skips non-portable
/// ones). `remove` keys off the same set so an unfiltered name list can never delete
/// a user server sharing a name with one we declared but never wrote.
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
}

/// `commands/hello.md` -> `<plugin>-hello`; a nested path flattens (`a/b.md` ->
/// `<plugin>-a-b`). The plugin prefix keeps the dir identifiable as ours for an
/// exact `remove` and clear of any devin built-in profile name.
fn namespaced(plugin: &str, rel: &str, prefix: &str) -> String {
    let stripped = rel.strip_prefix(prefix).unwrap_or(rel);
    let stem = stripped.strip_suffix(".md").unwrap_or(stripped);
    format!("{plugin}-{}", stem.replace(['/', '\\'], "-"))
}

// --- hooks -------------------------------------------------------------------

/// Map a CC hook event to devin's. Devin reuses CC's event names, so the shared
/// set is identity; events devin does not host (`PreCompact`, `Notification`,
/// `SubagentStop`) are skipped rather than written under a guessed name.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "SessionStart" => Some("SessionStart"),
        "SessionEnd" => Some("SessionEnd"),
        "UserPromptSubmit" => Some("UserPromptSubmit"),
        "PreToolUse" => Some("PreToolUse"),
        "PostToolUse" => Some("PostToolUse"),
        "Stop" => Some("Stop"),
        _ => None,
    }
}

/// A `${CLAUDE_PLUGIN_ROOT}` reference only expands inside Claude Code's own hook
/// runner; devin has no equivalent substitution, so such a command would spawn the
/// literal, unexpanded token. Mirrors `McpServer::is_portable` (applied locally:
/// `HookBinding` has no such method in the shared components IR).
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

/// Add-if-absent our hook groups under each mapped event in the config's `hooks`
/// key, leaving the user's own groups in place. Idempotent: a group already present
/// (deep-equal) is not re-added. Non-portable hooks and events with no devin analog
/// are skipped, same as mcp servers. Skips the whole edit when nothing is writable
/// so no empty `"hooks": {}` key is created for zero writes.
fn reconcile_hooks(config: &Path, hooks: &[HookBinding]) -> Result<bool> {
    let writable: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|event| (event, h))).collect();
    if writable.is_empty() {
        return Ok(false);
    }
    json_edit(config, |root| {
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
/// mirrors `reconcile_hooks`'s writable filter exactly (portable AND mapped to a
/// devin event) — a command string from an unmapped event (never written here) must
/// never be treated as ours to remove.
fn remove_hooks(config: &Path, hooks: &[HookBinding]) -> Result<bool> {
    if !config.exists() {
        return Ok(false);
    }
    let ours: BTreeSet<&str> =
        hooks.iter().filter(|h| hook_is_portable(h) && map_event(&h.event).is_some()).map(|h| h.command.as_str()).collect();
    json_edit(config, |root| {
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

// --- skills / agents ---------------------------------------------------------

/// Render a devin skill (`SKILL.md`) or subagent (`AGENT.md`): a namespaced `name`,
/// the CC doc's remaining frontmatter (`description`/`model`/… verbatim), then the
/// body. Devin reads YAML frontmatter + a markdown system prompt — the same shape
/// as the CC source — so translation is a re-emit with an ownership-safe `name`.
/// Deterministic (so a re-reconcile is byte-identical).
fn render_doc(plugin: &str, rel: &str, prefix: &str, doc: &MarkdownDoc) -> String {
    let mut out = String::from("---\n");
    let _ = writeln!(out, "name: {}", yaml_scalar(&namespaced(plugin, rel, prefix)));
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
        // Non-scalar frontmatter is out of the CC command/agent shape; stringify it
        // and quote so it can never break the document.
        other => yaml_scalar(&other.to_string()),
    }
}

/// A YAML scalar: bare when it cannot be misparsed as a flow/indicator token, else
/// a double-quoted string with the minimal escapes.
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

fn report_checks(backend: &DevinBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "devin detected", status: CheckStatus::Ok("`devin` on PATH or a devin config dir present".into()) }
    } else {
        DoctorCheck {
            name: "devin detected",
            status: CheckStatus::Fail {
                problem: "devin not detected".into(),
                fix: "install it with `curl -fsSL https://cli.devin.ai/install.sh | bash`".into(),
            },
        }
    });

    let base = match config_base(&Scope::User) {
        Ok(base) => base,
        Err(e) => {
            checks.push(DoctorCheck { name: "config file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };
    let config = base.join("config.json");

    let root = match fs::read(&config) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "config file", status: CheckStatus::Ok(format!("{} parses", config.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "config file",
                    status: CheckStatus::Fail {
                        problem: format!("{} does not parse: {e}", config.display()),
                        fix: "fix the JSON syntax or remove the file".into(),
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
    checks.push(check_docs_present("skills", "SKILL.md", "commands/", &comp.commands, plugin.name, &base));
    checks.push(check_docs_present("agents", "AGENT.md", "agents/", &comp.agents, plugin.name, &base));

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
                problem: format!("mcp server(s) not in config.json: {}", missing.join(", ")),
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

fn check_docs_present(subdir: &str, file: &str, prefix: &str, docs: &[MarkdownDoc], plugin: &str, base: &Path) -> DoctorCheck {
    // A shared name so the doctor fan-out's `<id>: ` prefix stays the whole label.
    let name: &'static str = if subdir == "skills" { "translated skills present" } else { "translated subagents present" };
    if docs.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok(format!("no {subdir} to translate")) };
    }
    let missing: Vec<String> =
        docs.iter().map(|d| namespaced(plugin, &d.rel, prefix)).filter(|dir| !base.join(subdir).join(dir).join(file).exists()).collect();
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
