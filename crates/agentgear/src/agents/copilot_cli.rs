//! The GitHub Copilot CLI backend: a full translate into copilot's own user-level
//! config, rooted at `~/.copilot/` (override `COPILOT_HOME`). MCP is bespoke json
//! (`mcp-config.json` `mcpServers.<name>`, copilot's `type:"local"` + `tools:["*"]`
//! shape — not the shared `type:"stdio"` renderer); hooks land as ONE file we own
//! outright at `hooks/<plugin>.json` (rendered deterministically, deleted whole on
//! remove); CC agent defs become `agents/<plugin>-<name>.agent.md`. Every mcp key is
//! our own server name and every file is plugin-name-prefixed, so `remove` is exact
//! and a second reconcile is a true `NoOp`.
//!
//! Two ownership boundaries worth stating (see `docs/harness/copilot-cli.md`):
//! - the repo-level `.github/hooks/*.json` surface is deliberately NOT touched here —
//!   it belongs to the `vscode-copilot` backend, so the two never double-write one
//!   file. This backend is user-scope only.
//! - copilot's CLI has no custom-slash-command file surface (a VS Code-only feature,
//!   open upstream FRs), so CC commands are skipped rather than written where the CLI
//!   would never read them.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::cchooks::hook_is_portable;
use super::confedit::{json_edit, json_obj_at, remove_file_idem, write_file_idem};
use super::report;
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

/// The top-level object key in `mcp-config.json`; shared by every mcp helper.
const MCP_KEY: &str = "mcpServers";

pub(crate) struct CopilotCliBackend;

impl AgentBackend for CopilotCliBackend {
    fn id(&self) -> &'static str {
        "copilot-cli"
    }

    fn detect(&self) -> bool {
        // `COPILOT_HOME` (copilot's documented relocation env) is honored before
        // `~/.copilot`, so a test setting either redirects config + detection; the
        // `copilot` CLI on PATH is a bonus but its absence never implies uninstalled.
        which::which("copilot").is_ok() || copilot_home_opt().is_some_and(|d| d.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // User scope only: copilot has no project-level MCP/hook config file for the
        // CLI (open upstream FRs). Commands are skipped (no CLI surface), so the
        // honest flags are mcp + hooks.
        Capabilities { plugins: false, mcp: true, hooks: true, scopes: &["user"] }
    }

    fn probe(&self, plugin: &Plugin, _scope: &Scope) -> Result<BackendState> {
        // Ownership is defined by our mcp server keys (the canonical "are we here"
        // signal); `probe_mcp` returns Healthy — never Absent — for an mcp-less
        // plugin, so a present marker is never dropped. Source::Embedded is the only
        // steady-state source for a non-CC backend (github unsupported, path is
        // install-only), mirroring the claude probe keying on compile-time metadata.
        let comp = plugin.components(&Source::Embedded)?;
        probe_mcp(&mcp_config()?, &comp.mcp_servers)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, _scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let home = copilot_home()?;

        let mut changed = false;
        changed |= reconcile_mcp(&home.join("mcp-config.json"), &comp.mcp_servers)?;
        changed |= reconcile_hooks(&hooks_file(&home, plugin.name), &comp.hooks)?;

        let agents = home.join("agents");
        for doc in &comp.agents {
            changed |= write_file_idem(&agents.join(agent_file(plugin.name, doc)), render_agent(plugin.name, doc).as_bytes())?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, _scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let home = copilot_home()?;

        let mut changed = false;
        changed |= remove_mcp(&home.join("mcp-config.json"), &portable_names(&comp.mcp_servers))?;
        changed |= remove_hooks(&hooks_file(&home, plugin.name), &comp.hooks)?;

        // `agents/` is shared with the user's own agent files, so we delete only our
        // plugin-prefixed files by name (never a `remove_dir_all`).
        let agents = home.join("agents");
        for doc in &comp.agents {
            changed |= remove_file_idem(&agents.join(agent_file(plugin.name, doc)))?;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The copilot home dir, honoring `COPILOT_HOME` (its documented override) then
/// `~/.copilot`. `_opt` never errors so `detect` can call it; a HOME-based fallback
/// means a test redirecting `HOME`/`COPILOT_HOME` also redirects detection.
fn copilot_home_opt() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("COPILOT_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir().map(|h| h.join(".copilot"))
}

fn copilot_home() -> Result<PathBuf> {
    copilot_home_opt().ok_or_else(|| Error::Tree("no home directory (HOME and COPILOT_HOME unset); cannot locate ~/.copilot".into()))
}

fn mcp_config() -> Result<PathBuf> {
    Ok(copilot_home()?.join("mcp-config.json"))
}

/// The single hooks file we own outright: `<home>/hooks/<plugin>.json`. Copilot
/// loads every `hooks/*.json` alphabetically, so a plugin-named file never collides
/// with the user's own hook files — remove drops it whole.
fn hooks_file(home: &Path, plugin: &str) -> PathBuf {
    home.join("hooks").join(format!("{plugin}.json"))
}

/// `agents/ez-helper.md` -> `<plugin>-ez-helper.agent.md`. Copilot scans
/// `agents/*.agent.md` (flat), so any nested path flattens; the plugin prefix keeps
/// the file identifiably ours for an exact `remove`.
fn agent_file(plugin: &str, doc: &MarkdownDoc) -> String {
    format!("{plugin}-{}.agent.md", flat_stem(&doc.rel, "agents"))
}

fn flat_stem(rel: &str, subdir: &str) -> String {
    let stripped = rel.strip_prefix(subdir).unwrap_or(rel).trim_start_matches('/');
    stripped.strip_suffix(".md").unwrap_or(stripped).replace(['/', '\\'], "-")
}

/// Server names `reconcile_mcp` actually writes (non-portable ones are skipped).
/// `remove` keys off the same set so it never deletes a user server that happens to
/// share a name with one we declared but never wrote.
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
}

// --- mcp (bespoke) -----------------------------------------------------------

/// copilot's server body: `type` is `local`/`http`/`sse` (not CC's `stdio`), and a
/// `tools:["*"]` field selects which tools the server exposes (explicit `["*"]` = all,
/// so an omitted-means-none build still gets every tool; the bare string `"*"` fails
/// copilot's parse and voids the whole file). Deterministic so a re-reconcile is
/// byte-identical -> a true `NoOp`.
fn render_mcp_server(server: &McpServer) -> Value {
    let mut obj = Map::new();
    match &server.kind {
        McpKind::Stdio => {
            obj.insert("type".into(), Value::from("local"));
            obj.insert("command".into(), Value::from(server.command.clone()));
            obj.insert("args".into(), Value::from(server.args.clone()));
            obj.insert("env".into(), env_value(server));
        }
        McpKind::Http { url } => remote(&mut obj, "http", url),
        McpKind::Sse { url } => remote(&mut obj, "sse", url),
    }
    obj.insert("tools".into(), Value::from(vec!["*"]));
    Value::Object(obj)
}

fn remote(obj: &mut Map<String, Value>, kind: &str, url: &str) {
    obj.insert("type".into(), Value::from(kind));
    obj.insert("url".into(), Value::from(url));
    obj.insert("headers".into(), Value::Object(Map::new()));
}

fn env_value(server: &McpServer) -> Value {
    Value::Object(server.env.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect())
}

/// Insert/update exactly our servers under `mcpServers`, leaving the user's own
/// keys. Skips the write entirely (no empty `mcpServers` key) when the plugin
/// declares no portable server.
fn reconcile_mcp(config: &Path, servers: &[McpServer]) -> Result<bool> {
    let portable: Vec<&McpServer> = servers.iter().filter(|s| s.is_portable()).collect();
    if portable.is_empty() {
        return Ok(false);
    }
    json_edit(config, |root| {
        let obj = json_obj_at(root, &[MCP_KEY]);
        for server in &portable {
            obj.insert(server.name.clone(), render_mcp_server(server));
        }
        Ok(())
    })
}

/// `Absent` if none of our servers are present; `Healthy` if all present and
/// byte-matching our render; `NeedsRepair` otherwise. `Healthy` (not `Absent`) when
/// the plugin declares no portable server, so a present marker is not dropped.
/// Copilot has no per-server disable flag we set, so there is no `Disabled` state.
fn probe_mcp(config: &Path, servers: &[McpServer]) -> Result<BackendState> {
    // Before the file read: an mcp-less plugin is Healthy even when reconcile never
    // wrote the file (it early-returns on an empty portable set), so a missing file
    // must not short-circuit to Absent and drop a present marker.
    let portable: Vec<&McpServer> = servers.iter().filter(|s| s.is_portable()).collect();
    if portable.is_empty() {
        return Ok(BackendState::Healthy);
    }
    let bytes = match fs::read(config) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BackendState::Absent),
        Err(source) => return Err(Error::Io { context: format!("reading {}", config.display()), source }),
    };
    let root: Value =
        serde_json::from_slice(&bytes).map_err(|e| Error::Config { path: config.display().to_string(), detail: e.to_string() })?;

    let obj = root.get(MCP_KEY).and_then(Value::as_object);
    let mut present = 0usize;
    let mut matching = 0usize;
    for server in &portable {
        if let Some(existing) = obj.and_then(|o| o.get(&server.name)) {
            present += 1;
            if *existing == render_mcp_server(server) {
                matching += 1;
            }
        }
    }
    Ok(if present == 0 {
        BackendState::Absent
    } else if matching == portable.len() {
        BackendState::Healthy
    } else {
        BackendState::NeedsRepair
    })
}

/// Remove exactly our server keys under `mcpServers`, leaving others. Conservatively
/// leaves an emptied object in place rather than dropping the file.
fn remove_mcp(config: &Path, names: &[&str]) -> Result<bool> {
    if !config.exists() || names.is_empty() {
        return Ok(false);
    }
    json_edit(config, |root| {
        if let Some(obj) = root.get_mut(MCP_KEY).and_then(Value::as_object_mut) {
            for name in names {
                obj.remove(*name);
            }
        }
        Ok(())
    })
}

// --- hooks (whole file we own) -----------------------------------------------

/// Map a CC hook event to copilot's own camelCase event name (verified against the
/// hooks-configuration reference). Events copilot does not define — and copilot-only
/// events with no CC analog — are skipped, never written under a guessed name.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "SessionStart" => Some("sessionStart"),
        "SessionEnd" => Some("sessionEnd"),
        "UserPromptSubmit" => Some("userPromptSubmitted"),
        "PreToolUse" => Some("preToolUse"),
        "PostToolUse" => Some("postToolUse"),
        "PreCompact" => Some("preCompact"),
        "Stop" => Some("agentStop"),
        "SubagentStop" => Some("subagentStop"),
        "Notification" => Some("notification"),
        _ => None,
    }
}

/// The (copilot-event, hook) pairs we actually write: portable hooks whose CC event
/// has a copilot analog. Both `reconcile_hooks` and `remove_hooks` key off this so
/// they stay in lockstep (we never delete a file we would not have written).
fn writable_hooks(hooks: &[HookBinding]) -> Vec<(&'static str, &HookBinding)> {
    hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|e| (e, h))).collect()
}

/// A single copilot hook handler: `type:"command"` with the shell string under
/// `bash` (copilot's key, not CC's `command`); an optional tool-name-regex matcher.
fn render_hook_entry(hook: &HookBinding) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), Value::from("command"));
    obj.insert("bash".into(), Value::from(hook.command.clone()));
    if let Some(matcher) = &hook.matcher {
        obj.insert("matcher".into(), Value::from(matcher.clone()));
    }
    Value::Object(obj)
}

/// Render the whole `hooks/<plugin>.json` file: copilot's `{version, disableAllHooks,
/// hooks:{event:[...]}}` shape, events in first-seen order. We own the file outright,
/// so it is rendered from scratch (not merged) and deterministic for idempotency.
fn render_hooks_file(writable: &[(&'static str, &HookBinding)]) -> Result<Vec<u8>> {
    let mut events = Map::new();
    for (event, hook) in writable {
        let list = events.entry((*event).to_string()).or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(arr) = list {
            arr.push(render_hook_entry(hook));
        }
    }
    let mut root = Map::new();
    root.insert("version".into(), Value::from(1));
    root.insert("disableAllHooks".into(), Value::Bool(false));
    root.insert("hooks".into(), Value::Object(events));

    let mut bytes =
        serde_json::to_vec_pretty(&Value::Object(root)).map_err(|source| Error::Json { what: "copilot hooks".into(), source })?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Write our owned hooks file. Skips the write (and never creates the file) when the
/// plugin has no writable hook, so an mcp-only plugin leaves no empty artifact.
fn reconcile_hooks(path: &Path, hooks: &[HookBinding]) -> Result<bool> {
    let writable = writable_hooks(hooks);
    if writable.is_empty() {
        return Ok(false);
    }
    write_file_idem(path, &render_hooks_file(&writable)?)
}

/// Delete our owned hooks file. Gated on the same `writable_hooks` set as the writer:
/// with nothing to write we never created the file, so a same-named file we did not
/// author is left untouched.
fn remove_hooks(path: &Path, hooks: &[HookBinding]) -> Result<bool> {
    if writable_hooks(hooks).is_empty() {
        return Ok(false);
    }
    remove_file_idem(path)
}

// --- agents ------------------------------------------------------------------

/// Render a CC agent def as a copilot custom-agent file (`agents/*.agent.md`). Only
/// the confirmed frontmatter fields are emitted: `name` (plugin-prefixed so two
/// plugins never collide and it stays identifiably ours) and `description`. CC's
/// `model` alias and `tools` list are dropped — copilot's frontmatter schema for
/// them is not documented, so writing a guessed shape risks a file copilot rejects.
/// Deterministic so a re-reconcile is byte-identical.
fn render_agent(plugin: &str, doc: &MarkdownDoc) -> String {
    let name = doc.frontmatter.get("name").and_then(Value::as_str).unwrap_or(doc.name.as_str());
    let mut out = String::from("---\n");
    // JSON-quote the prefixed name like the description below: a YAML-special char in
    // the name (or file-stem fallback) would otherwise produce frontmatter copilot rejects.
    out.push_str("name: ");
    out.push_str(&Value::String(format!("{plugin}-{name}")).to_string());
    out.push('\n');
    if let Some(desc) = doc.frontmatter.get("description").and_then(Value::as_str) {
        // JSON-quote keeps a colon/quote in the description from breaking the YAML
        // scalar (JSON double-quoted strings are valid YAML flow scalars).
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

fn report_checks(backend: &CopilotCliBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "copilot-cli detected", status: CheckStatus::Ok("`copilot` on PATH or ~/.copilot present".into()) }
    } else {
        DoctorCheck {
            name: "copilot-cli detected",
            status: CheckStatus::Fail {
                problem: "GitHub Copilot CLI not detected".into(),
                fix: "install it with `npm install -g @github/copilot`".into(),
            },
        }
    });

    let home = match copilot_home() {
        Ok(home) => home,
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp-config.json", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };
    let config = home.join("mcp-config.json");

    let root = report::read_json_config(&mut checks, "mcp-config.json", &config);

    let Some(comp) = report::components(&mut checks, plugin, source) else {
        return checks;
    };

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcpServers"],
        "not in mcp-config.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    checks.push(check_hooks_present(&comp.hooks, &hooks_file(&home, plugin.name)));
    checks.push(check_agents_present(&comp.agents, &home.join("agents"), plugin.name));

    checks
}

fn check_hooks_present(hooks: &[HookBinding], hooks_json: &Path) -> DoctorCheck {
    let name = "translated hooks present";
    let writable = writable_hooks(hooks);
    if writable.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no hooks to translate".into()) };
    }
    let text = fs::read_to_string(hooks_json).unwrap_or_default();
    let missing: Vec<&str> = writable.iter().map(|(_, h)| h.command.as_str()).filter(|c| !text.contains(c)).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} present", hooks_json.display())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("hook(s) missing from {}: {}", hooks_json.display(), missing.join(", ")),
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
    let missing: Vec<String> = agents.iter().map(|doc| agent_file(plugin, doc)).filter(|f| !agents_dir.join(f).exists()).collect();
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
#[path = "../../tests/unit/copilot_cli.rs"]
mod copilot_cli_tests;
