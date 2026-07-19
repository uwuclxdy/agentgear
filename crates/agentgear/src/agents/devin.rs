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
//! (`subagent_explore`/`subagent_general`) or a user's own. The plugin's own
//! `skills/` IR is a DISTINCT surface: it lands as bare `<name>/SKILL.md` in devin's
//! `.agents/skills` scan root (tagged for ownership), never colliding with the
//! commands-as-skills dirs above. See `docs/harness/devin.md` for the full mapping.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::cchooks::{hook_is_portable, render_hook_group};
use super::confedit::{json_edit, json_obj_at, write_file_idem, yaml_scalar};
use super::mcpjson::{self, RemoteShape, ServerShape};
use super::report;
use super::skillsdir;
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, IoContext, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct DevinBackend;

/// devin keys remote transport on a `transport` field and never reads `type`,
/// so the majority shape would silently load sse over http.
const SHAPE: ServerShape = ServerShape::plain().with_remote(RemoteShape::TransportKeyed);

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
        // Full surface: mcp + hooks + commands (as skills) + agents (subagents) + skills.
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: true,
            commands: true,
            agents: true,
            skills: true,
            instructions: false,
            scopes: &["user", "project"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Compose every surface (mcp + hooks in config.json, skill + subagent dirs), so
        // a dropped hook group or missing SKILL.md/AGENT.md behind healthy mcp keys reads
        // NeedsRepair. `source` is the one self_heal resolved for this agent (rehydrated
        // `--path`, else the compile-time default), so probe and reconcile render
        // identical bytes.
        let comp = plugin.components(source)?;
        let base = config_base(scope)?;
        let config = base.join("config.json");
        let mcp = mcpjson::probe_surface(&config, &["mcpServers"], &comp.mcp_servers, SHAPE)?;
        let hooks = report::probe_json_entries(&config, &hook_entries(&comp.hooks))?;
        let commands =
            report::probe_files(&expected_docs(&base, "skills", "SKILL.md", "commands/", plugin.name, &comp.commands), |_, _| true)?;
        let agents = report::probe_files(&expected_docs(&base, "agents", "AGENT.md", "agents/", plugin.name, &comp.agents), |_, _| true)?;
        // Plugin `skills/` (the SkillDir IR) is a distinct surface from CC-commands-as-
        // devin-skills above: it lands in devin's `.agents/skills` scan root, not the
        // `<config_base>/skills` dir the commands own.
        let skills = skillsdir::probe(&skillsdir::agents_skills_root(scope)?, plugin, &comp.skills)?;
        Ok(report::compose([mcp, hooks, commands, agents, skills].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let base = config_base(scope)?;
        let config = base.join("config.json");

        let mut changed = false;
        changed |= mcpjson::reconcile(&config, &["mcpServers"], &comp.mcp_servers, SHAPE)? != Outcome::NoOp;
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
        // Plugin skills land in the `.agents/skills` scan root (bare `<name>/`), kept
        // distinct from the `skills/<plugin>-<stem>/` dirs the CC commands own above.
        changed |= skillsdir::reconcile(&skillsdir::agents_skills_root(scope)?, plugin, &comp.skills)?;
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
        let comp = plugin.components(source)?;
        let base = config_base(scope)?;
        let config = base.join("config.json");

        let mut changed = false;
        changed |= mcpjson::remove(&config, &["mcpServers"], &comp.mcp_servers, SHAPE)? != Outcome::NoOp;
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
        changed |= skillsdir::remove(&skillsdir::agents_skills_root(scope)?, plugin, &comp.skills)?;
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

/// The `(array key_path, rendered group)` pairs `probe` checks are present under
/// `hooks.<event>`, mirroring `reconcile_hooks`'s writable filter exactly.
fn hook_entries(hooks: &[HookBinding]) -> Vec<(Vec<String>, Value)> {
    hooks
        .iter()
        .filter(|h| hook_is_portable(h))
        .filter_map(|h| map_event(&h.event).map(|event| (vec!["hooks".to_string(), event.to_string()], render_hook_group(h))))
        .collect()
}

/// The `(path, rendered bytes)` `SKILL.md`/`AGENT.md` files `probe` compares against
/// disk, keyed off the same `namespaced` dir + `render_doc` `reconcile` writes.
fn expected_docs(base: &Path, subdir: &str, file: &str, prefix: &str, plugin: &str, docs: &[MarkdownDoc]) -> Vec<(PathBuf, Vec<u8>)> {
    docs.iter()
        .map(|doc| {
            let path = base.join(subdir).join(namespaced(plugin, &doc.rel, prefix)).join(file);
            (path, render_doc(plugin, &doc.rel, prefix, doc).into_bytes())
        })
        .collect()
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

    let root = report::read_json_config(&mut checks, "config file", &config);

    let Some(comp) = report::components(&mut checks, plugin, source) else {
        return checks;
    };

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcpServers"],
        "not in config.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    checks.push(check_docs_present("skills", "SKILL.md", "commands/", &comp.commands, plugin.name, &base));
    checks.push(check_docs_present("agents", "AGENT.md", "agents/", &comp.agents, plugin.name, &base));

    checks
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
