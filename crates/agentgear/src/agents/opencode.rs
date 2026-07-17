//! The opencode backend: a full translate into opencode's own config. MCP is
//! bespoke (opencode's `command` is a single array + `type:"local"`, not CC's
//! split `command`/`args`), written under the top-level `mcp` object of
//! `~/.config/opencode/opencode.json` via `confedit::json_edit`. Commands copy
//! through as markdown (opencode's command format is CC-shaped); agents translate
//! into opencode subagent markdown (injecting `mode: subagent`). Every file we
//! emit is plugin-name-prefixed and every mcp key is our own server name, so
//! `remove` is exact and a second reconcile is a true `NoOp`.
//!
//! Skipped surfaces (see `docs/harness/opencode.md`): hooks (opencode has no
//! declarative shell-hook config — only an in-process JS/TS plugin API whose
//! CC-`UserPromptSubmit` analogue is unverified and version-volatile per the
//! brief), skills, and the CC `model` alias on agents (no reliable map to
//! opencode's `provider/model` ids).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::confedit::{json_edit, json_obj_at, remove_file_idem, write_file_idem, yaml_quote};
use super::report;
use super::{AgentBackend, BackendState};
use crate::components::{MarkdownDoc, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct OpencodeBackend;

impl AgentBackend for OpencodeBackend {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn detect(&self) -> bool {
        // `~/.config/opencode` is XDG-based, so a test redirecting `XDG_CONFIG_HOME`
        // (or `HOME`) redirects detection too; the `opencode` CLI on PATH is a bonus.
        which::which("opencode").is_ok() || dirs::config_dir().is_some_and(|c| c.join("opencode").is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // `hooks:false` — opencode's only hook surface is JS/TS plugins, not the
        // shell-command config CC-style hooks translate to (see the module doc).
        Capabilities { plugins: false, mcp: true, hooks: false, scopes: &["user", "project"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        // Ownership is defined by our mcp server keys (the canonical "are we here"
        // signal); `probe_mcp` returns Healthy — never Absent — for an mcp-less
        // plugin, so a present marker is never dropped. Source::Embedded is the only
        // steady-state source for a non-CC backend (github unsupported, path is
        // install-only), mirroring the claude probe keying on compile-time metadata.
        let comp = plugin.components(&Source::Embedded)?;
        probe_mcp(&config_file(scope)?, &comp.mcp_servers)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let config = config_file(scope)?;
        let base = surface_base(scope)?;

        let mut changed = false;
        changed |= reconcile_mcp(&config, &comp.mcp_servers, desired.reenable)?;
        // Commands are markdown + YAML frontmatter in both CC and opencode, so the
        // verbatim bytes are a valid opencode command (unknown CC keys are ignored).
        for doc in &comp.commands {
            changed |= write_file_idem(&doc_path(&base, "commands", plugin.name, doc), &doc.raw)?;
        }
        for doc in &comp.agents {
            changed |= write_file_idem(&doc_path(&base, "agents", plugin.name, doc), render_agent_md(doc).as_bytes())?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let config = config_file(scope)?;
        let base = surface_base(scope)?;

        let mut changed = false;
        changed |= remove_mcp(&config, &portable_names(&comp.mcp_servers))?;
        for doc in &comp.commands {
            changed |= remove_file_idem(&doc_path(&base, "commands", plugin.name, doc))?;
        }
        for doc in &comp.agents {
            changed |= remove_file_idem(&doc_path(&base, "agents", plugin.name, doc))?;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The user-scope `~/.config/opencode` dir (XDG-honoring). A missing config home
/// is a clear, actionable error rather than a silent skip.
fn opencode_config_dir() -> Result<PathBuf> {
    dirs::config_dir()
        .map(|c| c.join("opencode"))
        .ok_or_else(|| Error::Tree("no config directory (HOME and XDG_CONFIG_HOME unset); cannot locate ~/.config/opencode".into()))
}

/// The `opencode.json` we read-modify-write for a scope: `~/.config/opencode/
/// opencode.json` (user) or `<cwd>/opencode.json` (project — at the project root,
/// beside the `.opencode/` dir, per the brief).
fn config_file(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(opencode_config_dir()?.join("opencode.json")),
        Scope::Project { path } => Ok(path.join("opencode.json")),
    }
}

/// The base dir holding the `commands/` + `agents/` surface dirs: `~/.config/
/// opencode` (user) or `<cwd>/.opencode` (project). Note the project config file
/// sits at the root while its surface dirs live under `.opencode/`.
fn surface_base(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => opencode_config_dir(),
        Scope::Project { path } => Ok(path.join(".opencode")),
    }
}

/// The on-disk path for a translated doc under `<base>/<subdir>/`: plugin-name
/// prefixed and flattened (subdir separators -> `-`) into one file. A flat,
/// prefixed name is discoverable regardless of whether opencode recurses command/
/// agent subdirectories (unverified in the brief) and stays identifiably ours.
fn doc_path(base: &Path, subdir: &str, plugin: &str, doc: &MarkdownDoc) -> PathBuf {
    let stem =
        doc.rel.strip_prefix(subdir).unwrap_or(&doc.rel).trim_start_matches('/').strip_suffix(".md").unwrap_or(&doc.rel).replace('/', "-");
    base.join(subdir).join(format!("{plugin}-{stem}.md"))
}

/// Server names `reconcile_mcp` actually writes (non-portable ones are skipped).
/// `remove` keys off the same set so it never deletes a user server that happens
/// to share a name with one we declared but never wrote.
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
}

// --- mcp (bespoke) -----------------------------------------------------------

/// opencode's server body: `command` is a single array (`[cmd, ...args]`), `type`
/// is `local`/`remote` (not CC's `stdio`/`sse`/`http`). `enabled` is opencode's own
/// per-server on/off flag (unlike the shared json family, which has none) -
/// deterministic per `enabled` so a re-reconcile is byte-identical -> a true `NoOp`.
fn render_mcp_server(server: &McpServer, enabled: bool) -> Value {
    match &server.kind {
        McpKind::Stdio => {
            let mut command = Vec::with_capacity(1 + server.args.len());
            command.push(Value::from(server.command.clone()));
            command.extend(server.args.iter().map(|a| Value::from(a.clone())));
            let env: Map<String, Value> = server.env.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect();
            let mut obj = Map::new();
            obj.insert("type".into(), Value::from("local"));
            obj.insert("command".into(), Value::Array(command));
            obj.insert("enabled".into(), Value::Bool(enabled));
            obj.insert("environment".into(), Value::Object(env));
            Value::Object(obj)
        }
        // opencode collapses SSE/HTTP into one `remote` type keyed by `url`.
        McpKind::Http { url } | McpKind::Sse { url } => {
            let mut obj = Map::new();
            obj.insert("type".into(), Value::from("remote"));
            obj.insert("url".into(), Value::from(url.clone()));
            obj.insert("enabled".into(), Value::Bool(enabled));
            Value::Object(obj)
        }
    }
}

/// Insert/update exactly our servers under the top-level `mcp` object, leaving
/// the user's own keys. Skips the write entirely (no empty `mcp` key) when the
/// plugin declares no portable server. `reenable=false` (self_heal) preserves an
/// existing explicit `enabled:false` on our own key instead of forcing it back on,
/// since opencode's `enabled` flag is a real per-server disable a user can set and
/// the foundation's never-re-enable invariant applies to it exactly like CC's.
/// `reenable=true` (an explicit install/update) always re-enables, mirroring
/// `claude.rs`'s `entry.enabled == Some(false)` handling.
fn reconcile_mcp(config: &Path, servers: &[McpServer], reenable: bool) -> Result<bool> {
    let portable: Vec<&McpServer> = servers.iter().filter(|s| s.is_portable()).collect();
    if portable.is_empty() {
        return Ok(false);
    }
    json_edit(config, |root| {
        let obj = json_obj_at(root, &["mcp"]);
        for server in &portable {
            let currently_disabled = obj.get(&server.name).and_then(|v| v.get("enabled")).and_then(Value::as_bool) == Some(false);
            let enabled = reenable || !currently_disabled;
            obj.insert(server.name.clone(), render_mcp_server(server, enabled));
        }
        Ok(())
    })
}

/// Remove exactly our server keys under `mcp`, leaving others. Conservatively
/// leaves an emptied `mcp` object in place rather than dropping the file.
fn remove_mcp(config: &Path, names: &[&str]) -> Result<bool> {
    if !config.exists() || names.is_empty() {
        return Ok(false);
    }
    json_edit(config, |root| {
        if let Some(obj) = root.get_mut("mcp").and_then(Value::as_object_mut) {
            for name in names {
                obj.remove(*name);
            }
        }
        Ok(())
    })
}

/// `Absent` if none of our servers are present; `Disabled` if all present
/// servers exactly match our render with `enabled:false` (a user's deliberate
/// opencode-level disable - self_heal must never flip it back, same invariant as
/// CC's plugin disable); `Healthy` if all present and byte-matching our enabled
/// render; `NeedsRepair` otherwise (drifted, or a mix of enabled/disabled/drifted
/// across multiple servers). `Healthy` (not `Absent`) when the plugin declares no
/// portable server, so a present marker is not dropped.
fn probe_mcp(config: &Path, servers: &[McpServer]) -> Result<BackendState> {
    let bytes = match fs::read(config) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BackendState::Absent),
        Err(source) => return Err(Error::Io { context: format!("reading {}", config.display()), source }),
    };
    let root: Value =
        serde_json::from_slice(&bytes).map_err(|e| Error::Config { path: config.display().to_string(), detail: e.to_string() })?;

    let portable: Vec<&McpServer> = servers.iter().filter(|s| s.is_portable()).collect();
    if portable.is_empty() {
        return Ok(BackendState::Healthy);
    }
    let obj = root.get("mcp").and_then(Value::as_object);
    let mut present = 0usize;
    let mut enabled = 0usize;
    let mut disabled = 0usize;
    for server in &portable {
        if let Some(existing) = obj.and_then(|o| o.get(&server.name)) {
            present += 1;
            if *existing == render_mcp_server(server, true) {
                enabled += 1;
            } else if *existing == render_mcp_server(server, false) {
                disabled += 1;
            }
        }
    }
    Ok(if present == 0 {
        BackendState::Absent
    } else if disabled == portable.len() {
        BackendState::Disabled
    } else if enabled == portable.len() {
        BackendState::Healthy
    } else {
        BackendState::NeedsRepair
    })
}

// --- agents ------------------------------------------------------------------

/// Render a CC agent doc as opencode subagent markdown. CC agents are always
/// subagents (invoked via the Task tool), so `mode: subagent` is injected;
/// opencode would otherwise treat a mode-less file as a primary agent. The CC
/// `model` alias (`sonnet`/`opus`) is dropped — it has no reliable map to
/// opencode's `provider/model` ids, so opencode's own default is used instead.
fn render_agent_md(doc: &MarkdownDoc) -> String {
    let mut out = String::from("---\n");
    if let Some(desc) = doc.frontmatter.get("description").and_then(Value::as_str) {
        let _ = writeln!(out, "description: {}", yaml_quote(desc));
    }
    out.push_str("mode: subagent\n---\n\n");
    out.push_str(doc.body.trim_start_matches(['\n', '\r']));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &OpencodeBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "opencode detected", status: CheckStatus::Ok("`opencode` on PATH or ~/.config/opencode present".into()) }
    } else {
        DoctorCheck {
            name: "opencode detected",
            status: CheckStatus::Fail {
                problem: "opencode CLI not detected".into(),
                fix: "install it with `npm install -g opencode-ai`".into(),
            },
        }
    });

    let config = match config_file(&Scope::User) {
        Ok(config) => config,
        Err(e) => {
            checks.push(DoctorCheck { name: "config file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };

    let root = report::read_json_config(&mut checks, "config file", &config);

    let Some(comp) = report::components(&mut checks, plugin, source) else {
        return checks;
    };

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcp"],
        "not under `mcp` in opencode.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));

    let base = match surface_base(&Scope::User) {
        Ok(base) => base,
        Err(e) => {
            checks.push(DoctorCheck { name: "translated files present", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };
    checks.push(check_docs_present("translated commands present", &comp.commands, &base, "commands", plugin.name));
    checks.push(check_docs_present("translated agents present", &comp.agents, &base, "agents", plugin.name));

    checks
}

fn check_docs_present(name: &'static str, docs: &[MarkdownDoc], base: &Path, subdir: &str, plugin: &str) -> DoctorCheck {
    if docs.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("nothing to translate".into()) };
    }
    let missing: Vec<String> =
        docs.iter().map(|doc| doc_path(base, subdir, plugin, doc)).filter(|p| !p.exists()).map(|p| p.display().to_string()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} file(s) present", docs.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail { problem: format!("file(s) missing: {}", missing.join(", ")), fix: "run the host's `setup`".into() },
        }
    }
}
