//! The kilo backend: a full translate into the Kilo Code CLI's own config
//! (`@kilocode/cli`, binary `kilo`). The CLI is an opencode fork, so its mcp
//! shape is opencode's, NOT the CC `mcpServers` family: `command` is a single
//! array (`[cmd, ...args]`), env is `environment`, transport is
//! `type:"local"`/`"remote"` (remote = http/sse, keyed by `url` + `headers`).
//! Servers live under the root `mcp` object of `~/.config/kilo/kilo.json`, written
//! via `confedit::json_edit`. Commands copy through as markdown (kilo's command
//! format is CC-shaped); agents translate into kilo subagent markdown (`mode:
//! subagent`, an opencode-inherited field). Every file we emit is
//! plugin-name-prefixed and every mcp key is our own server name, so `remove` is
//! exact and a second reconcile is a true `NoOp`.
//!
//! Skills land as bare `<base>/skills/<name>/SKILL.md` (kilo scans `{skill,skills}/`
//! in every discovered config dir, both scopes), tagged for ownership; kilo requires
//! `name`+`description`, which the shared renderer ensures.
//!
//! Skipped surface (see `docs/harness/kilo.md`): hooks (kilo has no config-file
//! shell-hook surface — an open feature request, kilocode#5827, asks for
//! opencode-style lifecycle hooks).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::confedit::{json_edit, json_obj_at, json_prune_obj, json_remove, remove_file_idem, write_file_idem, yaml_quote};
use super::report;
use super::skillsdir;
use super::{AgentBackend, BackendState};
use crate::components::{MarkdownDoc, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct KiloBackend;

impl AgentBackend for KiloBackend {
    fn id(&self) -> &'static str {
        "kilo"
    }

    fn detect(&self) -> bool {
        // `~/.config/kilo` is XDG-based, so a test redirecting `XDG_CONFIG_HOME`
        // (or `HOME`) redirects detection too; the `kilo` CLI on PATH is a bonus.
        // The session env vars (KILOCODE_FEATURE, ...) are mid-unification per the
        // brief, so binary + config dir are the stable signals.
        which::which("kilo").is_ok() || kilo_config_dir_opt().is_some_and(|c| c.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // `hooks:false` — kilo has no config-file shell-hook surface (kilocode#5827).
        // mcp + commands + agents + skills translate.
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: false,
            commands: true,
            agents: true,
            skills: true,
            instructions: false,
            statusline: false,
            scopes: &["user", "project"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Compose every surface (mcp + the command/agent markdown files), so a missing
        // command or agent file behind a healthy mcp map reads NeedsRepair. `probe_mcp`
        // still carries the Disabled classification for a user-flipped `enabled:false`.
        // `source` is the one self_heal resolved for this agent (rehydrated `--path`,
        // else the compile-time default), so probe and reconcile render identical bytes.
        let comp = plugin.components(source)?.with_client(self.id());
        let mcp =
            if comp.mcp_servers.iter().any(|s| s.is_portable()) { Some(probe_mcp(&config_file(scope)?, &comp.mcp_servers)?) } else { None };
        let base = surface_base(scope)?;
        let commands =
            report::probe_files(&expected_docs(&base, "commands", plugin.name, &comp.commands, |doc| doc.raw.clone()), |_, _| true)?;
        let agents = report::probe_files(
            &expected_docs(&base, "agents", plugin.name, &comp.agents, |doc| render_agent_md(doc).into_bytes()),
            |_, _| true,
        )?;
        let skills = skillsdir::probe(&base.join("skills"), plugin, &comp.skills)?;
        Ok(report::compose([mcp, commands, agents, skills].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?.with_client(self.id());
        let config = config_file(scope)?;
        let base = surface_base(scope)?;

        let mut changed = false;
        changed |= reconcile_mcp(&config, &comp.mcp_servers, desired.reenable)?;
        // Commands are markdown + YAML frontmatter in both CC and kilo, so the
        // verbatim bytes are a valid kilo command (unknown CC keys are ignored).
        for doc in &comp.commands {
            changed |= write_file_idem(&doc_path(&base, "commands", plugin.name, doc), &doc.raw)?;
        }
        for doc in &comp.agents {
            changed |= write_file_idem(&doc_path(&base, "agents", plugin.name, doc), render_agent_md(doc).as_bytes())?;
        }
        changed |= skillsdir::reconcile(&base.join("skills"), plugin, &comp.skills)?;
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
        let comp = plugin.components(source)?.with_client(self.id());
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
        changed |= skillsdir::remove(&base.join("skills"), plugin, &comp.skills)?;
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The user-scope `~/.config/kilo` dir. `dirs::config_dir()` honors
/// `XDG_CONFIG_HOME` (Linux) and is `%APPDATA%` (Windows), but on macOS kilo (an
/// opencode-forked node CLI) uses `~/.config/kilo` too — not `dirs::config_dir()`'s
/// `~/Library/Application Support` default — so the macOS arm replicates dirs' own
/// XDG-or-home logic to match what kilo actually reads. `_opt` never errors so
/// `detect` can use it directly.
fn kilo_config_dir_opt() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .map(|c| c.join("kilo"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        dirs::config_dir().map(|c| c.join("kilo"))
    }
}

/// The user-scope config dir, or a clear, actionable error rather than a silent
/// skip when no config home is resolvable.
fn kilo_config_dir() -> Result<PathBuf> {
    kilo_config_dir_opt()
        .ok_or_else(|| Error::Tree("no config directory (XDG_CONFIG_HOME and HOME both unset); cannot locate ~/.config/kilo".into()))
}

/// The `kilo.json` we read-modify-write for a scope: `~/.config/kilo/kilo.json`
/// (user) or `<project>/.kilo/kilo.json` (project — kilo's documented project
/// config path, beside the `.kilo/` surface dirs).
fn config_file(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(kilo_config_dir()?.join("kilo.json")),
        Scope::Project { path } => Ok(path.join(".kilo").join("kilo.json")),
    }
}

/// The base dir holding the `commands/` + `agents/` surface dirs: `~/.config/kilo`
/// (user) or `<project>/.kilo` (project). Project config + surfaces both live under
/// `.kilo/`, so one base covers them.
fn surface_base(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => kilo_config_dir(),
        Scope::Project { path } => Ok(path.join(".kilo")),
    }
}

/// The on-disk path for a translated doc under `<base>/<subdir>/`: plugin-name
/// prefixed and flattened (subdir separators -> `-`) into one file. A flat,
/// prefixed name stays identifiably ours and discoverable regardless of whether
/// kilo recurses command/agent subdirectories.
fn doc_path(base: &Path, subdir: &str, plugin: &str, doc: &MarkdownDoc) -> PathBuf {
    let stem =
        doc.rel.strip_prefix(subdir).unwrap_or(&doc.rel).trim_start_matches('/').strip_suffix(".md").unwrap_or(&doc.rel).replace('/', "-");
    base.join(subdir).join(format!("{plugin}-{stem}.md"))
}

/// The `(path, rendered bytes)` files `probe` compares against disk for a surface
/// dir, keyed off the same `doc_path` + render `reconcile` writes.
fn expected_docs(
    base: &Path, subdir: &str, plugin: &str, docs: &[MarkdownDoc], render: impl Fn(&MarkdownDoc) -> Vec<u8>,
) -> Vec<(PathBuf, Vec<u8>)> {
    docs.iter().map(|doc| (doc_path(base, subdir, plugin, doc), render(doc))).collect()
}

/// Server names `reconcile_mcp` actually writes (non-portable ones are skipped).
/// `remove` keys off the same set so it never deletes a user server that happens
/// to share a name with one we declared but never wrote.
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
}

// --- mcp (bespoke, opencode-shaped) ------------------------------------------

/// kilo's server body (opencode-inherited): `command` is a single array (`[cmd,
/// ...args]`), env is `environment`, `type` is `local`/`remote` (not CC's
/// `stdio`/`sse`/`http`), remote carries `url` + `headers`. `enabled` is kilo's own
/// per-server on/off flag - deterministic per `enabled` so a re-reconcile is
/// byte-identical -> a true `NoOp`.
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
            obj.insert("environment".into(), Value::Object(env));
            obj.insert("enabled".into(), Value::Bool(enabled));
            Value::Object(obj)
        }
        // kilo collapses SSE/HTTP into one `remote` type keyed by `url` (+ headers).
        McpKind::Http { url } | McpKind::Sse { url } => {
            let mut obj = Map::new();
            obj.insert("type".into(), Value::from("remote"));
            obj.insert("url".into(), Value::from(url.clone()));
            obj.insert("headers".into(), Value::Object(Map::new()));
            obj.insert("enabled".into(), Value::Bool(enabled));
            Value::Object(obj)
        }
    }
}

/// Insert/update exactly our servers under the root `mcp` object, leaving the
/// user's own keys. Skips the write entirely (no empty `mcp` key) when the plugin
/// declares no portable server. `reenable=false` (self_heal) preserves an existing
/// explicit `enabled:false` on our own key rather than forcing it back on, since
/// kilo's `enabled` is a real per-server disable a user can set and the never-
/// re-enable invariant applies to it exactly like CC's. `reenable=true` (an
/// explicit install/update) always re-enables.
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

/// Remove exactly our server keys under `mcp`, leaving others. The `mcp` object goes
/// with our last key when our own removal is what emptied it, and the file goes with
/// an emptied root; a `mcp` the user had empty before us is untouched.
fn remove_mcp(config: &Path, names: &[&str]) -> Result<bool> {
    if !config.exists() || names.is_empty() {
        return Ok(false);
    }
    json_remove(config, |root| {
        json_prune_obj(root, &["mcp"], |obj| {
            for name in names {
                obj.remove(*name);
            }
            Ok(())
        })
        .map(|_| ())
    })
}

/// `Absent` if none of our servers are present; `Disabled` if all present servers
/// exactly match our render with `enabled:false` (a user's deliberate kilo-level
/// disable - self_heal must never flip it back); `Healthy` if all present and
/// byte-matching our enabled render; `NeedsRepair` otherwise (drifted, or a mix of
/// enabled/disabled/drifted). `Healthy` (not `Absent`) when the plugin declares no
/// portable server, so a present marker is not dropped.
fn probe_mcp(config: &Path, servers: &[McpServer]) -> Result<BackendState> {
    // No portable servers means we own nothing in the mcp config, so there is
    // nothing that could be "gone" -> Healthy (never Absent), regardless of whether
    // the file exists. Checking this before the read keeps self_heal from dropping a
    // present marker for a commands/agents-only plugin whose kilo.json never got created.
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

/// Render a CC agent doc as kilo subagent markdown. CC agents are always subagents
/// (invoked via the Task tool), so `mode: subagent` is injected (an opencode-
/// inherited field); kilo would otherwise treat a mode-less file as a primary
/// agent. The CC `model` alias (`sonnet`/`opus`) is dropped — it has no reliable
/// map to kilo's provider/model ids, so kilo's own default is used instead.
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

fn report_checks(backend: &KiloBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "kilo detected", status: CheckStatus::Ok("`kilo` on PATH or ~/.config/kilo present".into()) }
    } else {
        DoctorCheck {
            name: "kilo detected",
            status: CheckStatus::Fail {
                problem: "kilo CLI not detected".into(),
                fix: "install it with `npm install -g @kilocode/cli`".into(),
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

    let Some(comp) = report::components(&mut checks, plugin, source).map(|c| c.with_client(backend.id())) else {
        return checks;
    };

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcp"],
        "not under `mcp` in kilo.json",
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

#[cfg(test)]
#[path = "../../tests/unit/kilo.rs"]
mod kilo_tests;
