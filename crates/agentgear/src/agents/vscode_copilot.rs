//! The VS Code Copilot backend: GitHub Copilot Chat running inside VS Code. A
//! **project-scoped** translate into the repo's own files — there is no coherent
//! user-scope target (the profile-nested `mcp.json` path is OS/variant-ambiguous,
//! see `docs/harness/vscode-copilot.md`), so the orchestration skips this backend at
//! user scope and the lifecycle methods reject `Scope::User` rather than guess a path.
//!
//! MCP goes through the shared json renderer into `<project>/.vscode/mcp.json` under
//! the root key `servers` (NOT CC's `mcpServers`), `ServerShape::typed()` (VS Code
//! wants an explicit `"type":"stdio"`). Hooks land in a file we own entirely,
//! `<project>/.github/hooks/<plugin>.json`, with CC event names mapped to VS Code's
//! (identity for the 7 shared events; `SessionEnd`/`Notification` have no analog and
//! are skipped). CC agent defs become `<project>/.github/agents/<plugin>-<name>.agent.md`.
//! Everything we write is keyed by our server names or prefixed with the plugin name,
//! so `remove` is exact and a second reconcile is a true `NoOp`. Commands, skills,
//! and the user-scope profile config are skipped (see the harness doc).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::cchooks::hook_is_portable;
use super::confedit::{remove_file_idem, write_file_idem};
use super::mcpjson::{self, RemoteShape, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct VscodeCopilotBackend;

/// `.vscode/mcp.json` nests servers under a bare `servers` object (not CC's
/// `mcpServers`); stdio entries carry an explicit `"type":"stdio"`. Remote is
/// http-only: VS Code's parser collapses `type:"sse"` to http and its own writer
/// rewrites the stored key (permanent probe churn), so sse is skipped.
const MCP_KEY: &[&str] = &["servers"];
const SHAPE: ServerShape = ServerShape::typed().with_remote(RemoteShape::TypeUrlHeadersHttpOnly);

impl AgentBackend for VscodeCopilotBackend {
    fn id(&self) -> &'static str {
        "vscode-copilot"
    }

    fn detect(&self) -> bool {
        // VS Code Copilot is an extension of the `code` editor, not a standalone
        // binary; `~/.vscode` (its extensions/CLI home) is the host-agnostic
        // "installed here" signal. HOME-based via `dirs` so a test redirecting
        // `$HOME` also redirects detection; no session env var ships for it (the
        // proposed `VSCODE_COPILOT_TERMINAL` was never implemented — see the doc).
        which::which("code").is_ok()
            || which::which("code-insiders").is_ok()
            || dirs::home_dir().is_some_and(|h| h.join(".vscode").is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // Project-scope only: the sole documented, unambiguous config lives in the
        // repo (`.vscode/`, `.github/`). The fan-out skips this backend at user scope.
        Capabilities { plugins: false, mcp: true, hooks: true, scopes: &["project"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        // Ownership is our mcp server keys. Source::Embedded is the only steady-state
        // source for a non-CC backend (github unsupported, path install-only); the
        // shared probe returns Healthy — never Absent — for a plugin with no portable
        // servers, so a present marker is never dropped.
        let root = project_root(scope)?;
        let comp = plugin.components(&Source::Embedded)?;
        mcpjson::probe(&mcp_path(root), MCP_KEY, &comp.mcp_servers, SHAPE)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let root = project_root(scope)?;
        let comp = plugin.components(&desired.source)?;

        let mut changed = false;
        changed |= mcpjson::reconcile(&mcp_path(root), MCP_KEY, &comp.mcp_servers, SHAPE)? != Outcome::NoOp;
        changed |= reconcile_hooks(&hooks_path(root, plugin.name), &comp.hooks)?;

        let agent_root = agents_dir(root);
        for doc in &comp.agents {
            changed |= write_file_idem(&agent_root.join(agent_file(plugin.name, doc)), render_agent(plugin.name, doc).as_bytes())?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let root = project_root(scope)?;
        let comp = plugin.components(&Source::Embedded)?;

        let mut changed = false;
        // Key removal off the same portable set reconcile writes: an unfiltered name
        // could delete an unrelated user server sharing a name with a non-portable
        // entry we never wrote (e.g. a `${CLAUDE_PLUGIN_ROOT}`-bearing one).
        changed |= mcpjson::remove(&mcp_path(root), MCP_KEY, &comp.mcp_servers, SHAPE)? != Outcome::NoOp;
        // The hooks file is entirely ours (`<plugin>.json`), so a whole-file delete is exact.
        changed |= remove_file_idem(&hooks_path(root, plugin.name))?;

        let agent_root = agents_dir(root);
        for doc in &comp.agents {
            changed |= remove_file_idem(&agent_root.join(agent_file(plugin.name, doc)))?;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The project directory whose `.vscode`/`.github` files this backend owns. There
/// is no user-scope surface, so `Scope::User` is an error rather than a guessed
/// path (the fan-out already skips user scope; this guard is defensive).
fn project_root(scope: &Scope) -> Result<&Path> {
    match scope {
        Scope::Project { path } => Ok(path),
        Scope::User => {
            Err(Error::Tree("vscode-copilot is project-scoped; it has no user-scope config surface (install into a project scope)".into()))
        }
    }
}

/// `<project>/.vscode/mcp.json` — the workspace MCP config (shared with the user's
/// own servers, so writes merge by key).
fn mcp_path(root: &Path) -> PathBuf {
    root.join(".vscode").join("mcp.json")
}

/// `<project>/.github/hooks/<plugin>.json` — one file we own outright. The
/// `.github/hooks/` dir may hold the user's own or other tools' hook files; ours is
/// plugin-named, so we never touch theirs.
fn hooks_path(root: &Path, plugin: &str) -> PathBuf {
    root.join(".github").join("hooks").join(format!("{plugin}.json"))
}

/// `<project>/.github/agents/` — shared with the user's own agent files, so we only
/// ever write/delete plugin-prefixed `<plugin>-<name>.agent.md` entries by name.
fn agents_dir(root: &Path) -> PathBuf {
    root.join(".github").join("agents")
}

/// Server names `reconcile` actually writes (the shared renderer skips non-portable
/// ones); `remove` keys off the same set.
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
}

// --- hooks -------------------------------------------------------------------

/// Map a CC hook event to VS Code Copilot's own name. VS Code carries three hook
/// vocabularies; a `.github/hooks` file like ours parses as the `copilot` format,
/// whose resolver tries the camelCase map first and then falls back to accepting any
/// name from the canonical 10-event set verbatim. That set shares CC's PascalCase
/// spelling, so the map is identity for the 8 CC events it contains. `SessionEnd` is
/// among them (it is absent only from the narrower `vscode` vocabulary, which is why
/// it once looked analog-less). `Notification` is in no vocabulary at all and stays
/// skipped, never written under a guess.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "SessionStart" => Some("SessionStart"),
        "SessionEnd" => Some("SessionEnd"),
        "UserPromptSubmit" => Some("UserPromptSubmit"),
        "PreToolUse" => Some("PreToolUse"),
        "PostToolUse" => Some("PostToolUse"),
        "PreCompact" => Some("PreCompact"),
        "SubagentStop" => Some("SubagentStop"),
        "Stop" => Some("Stop"),
        _ => None,
    }
}

/// A VS Code hook entry: a flat `{type:"command", command}` object placed directly in
/// the event array. CC's `matcher` is dropped — VS Code's native hook entry has no
/// matcher field and ignores the value regardless, so hooks fire on every occurrence
/// of the event (VS Code has no per-tool hook filtering to translate into).
fn render_hook_entry(hook: &HookBinding) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), Value::from("command"));
    obj.insert("command".into(), Value::from(hook.command.clone()));
    Value::Object(obj)
}

/// Build the whole hooks-file document from the portable, mappable hooks, grouped by
/// their VS Code event (`{"hooks":{Event:[entry,...]}}`). Deterministic (events in
/// sorted order, entries in input order) so a re-render is byte-identical. `None` when
/// nothing survives, so no empty file is created.
fn render_hooks_file(hooks: &[HookBinding]) -> Option<Value> {
    let mut by_event: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    for hook in hooks.iter().filter(|h| hook_is_portable(h)) {
        if let Some(event) = map_event(&hook.event) {
            by_event.entry(event).or_default().push(render_hook_entry(hook));
        }
    }
    if by_event.is_empty() {
        return None;
    }
    let events: Map<String, Value> = by_event.into_iter().map(|(event, entries)| (event.to_string(), Value::Array(entries))).collect();
    let mut root = Map::new();
    root.insert("hooks".into(), Value::Object(events));
    Some(Value::Object(root))
}

/// Write the plugin's owned hooks file, or delete a stale one when the plugin now
/// declares no portable+mappable hooks (we own the whole file, so it must track the
/// plugin exactly). Idempotent via `write_file_idem`/`remove_file_idem`.
fn reconcile_hooks(path: &Path, hooks: &[HookBinding]) -> Result<bool> {
    match render_hooks_file(hooks) {
        Some(value) => {
            let mut bytes =
                serde_json::to_vec_pretty(&value).map_err(|source| Error::Json { what: "vscode-copilot hooks".into(), source })?;
            bytes.push(b'\n');
            write_file_idem(path, &bytes)
        }
        None => remove_file_idem(path),
    }
}

// --- agents ------------------------------------------------------------------

/// The VS Code agent id `<plugin>-<flattened stem>`. Both the `.agent.md` filename and
/// the emitted frontmatter `name` derive from this one value so they can never diverge:
/// VS Code prefers the frontmatter `name`, but our `remove`/`doctor` key on the
/// filename, so the two must agree. The plugin prefix keeps it identifiable as ours for
/// an exact `remove`; nested paths flatten since VS Code scans `.github/agents/*.agent.md`
/// (no subdir namespacing).
fn agent_name(plugin: &str, doc: &MarkdownDoc) -> String {
    format!("{plugin}-{}", flat_stem(&doc.rel, "agents/"))
}

/// `agents/ez-helper.md` -> `<plugin>-ez-helper.agent.md`.
fn agent_file(plugin: &str, doc: &MarkdownDoc) -> String {
    format!("{}.agent.md", agent_name(plugin, doc))
}

fn flat_stem(rel: &str, prefix: &str) -> String {
    let stripped = rel.strip_prefix(prefix).unwrap_or(rel);
    let stem = stripped.strip_suffix(".md").unwrap_or(stripped);
    stem.replace(['/', '\\'], "-")
}

/// Render a CC agent def as a VS Code `.agent.md` (markdown + YAML frontmatter). The
/// frontmatter `name` is `agent_name` (the plugin-prefixed file stem, not the CC `name`
/// field) so the emitted name always matches the `.agent.md` filename our lifecycle
/// keys on. `model` is dropped: CC's `sonnet`/`opus`/`haiku` aliases are not Copilot
/// model ids, and VS Code documents no portable "inherit" value, so omitting it lets
/// Copilot default. Deterministic so a re-reconcile is byte-identical.
fn render_agent(plugin: &str, doc: &MarkdownDoc) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str("name: ");
    out.push_str(&agent_name(plugin, doc));
    out.push('\n');
    if let Some(desc) = doc.frontmatter.get("description").and_then(Value::as_str) {
        // JSON-quote keeps a colon/quote in the description from breaking the YAML
        // scalar (a JSON double-quoted string is a valid YAML flow scalar).
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

/// `doctor` has no explicit project context, so a project-scoped backend reports
/// against the current working directory (the natural "am I set up in this repo"
/// question). Absent config there is a warning, not a failure.
fn report_checks(backend: &VscodeCopilotBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "vscode-copilot detected", status: CheckStatus::Ok("`code` on PATH or ~/.vscode present".into()) }
    } else {
        DoctorCheck {
            name: "vscode-copilot detected",
            status: CheckStatus::Warn("no `code` on PATH and no ~/.vscode; VS Code Copilot isn't set up here".into()),
        }
    });

    let root = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            checks
                .push(DoctorCheck { name: "mcp.json", status: CheckStatus::Warn(format!("could not resolve the current directory: {e}")) });
            return checks;
        }
    };
    let mcp = mcp_path(&root);

    let parsed = match fs::read(&mcp) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "mcp.json", status: CheckStatus::Ok(format!("{} parses", mcp.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "mcp.json",
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
                name: "mcp.json",
                status: CheckStatus::Warn(format!("{} does not exist (run setup in the project root)", mcp.display())),
            });
            None
        }
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp.json", status: CheckStatus::Warn(format!("could not read {}: {e}", mcp.display())) });
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

    checks.push(check_mcp_registered(&comp.mcp_servers, parsed.as_ref()));
    checks.push(check_mcp_command(&comp.mcp_servers));
    checks.push(check_agents_present(&comp.agents, &agents_dir(&root), plugin.name));

    checks
}

fn check_mcp_registered(servers: &[McpServer], root: Option<&Value>) -> DoctorCheck {
    let name = "mcp server registered";
    let portable = portable_names(servers);
    if portable.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no portable mcp servers to register".into()) };
    }
    let obj = root.and_then(|r| r.get("servers")).and_then(Value::as_object);
    let missing: Vec<&str> = portable.iter().copied().filter(|n| obj.is_none_or(|o| !o.contains_key(*n))).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} registered", portable.join(", "))) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp server(s) not in mcp.json: {}", missing.join(", ")),
                fix: "run the host's `setup` in the project root".into(),
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

fn check_agents_present(agents: &[MarkdownDoc], dir: &Path, plugin: &str) -> DoctorCheck {
    let name = "translated agents present";
    if agents.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no agents to translate".into()) };
    }
    let missing: Vec<String> = agents.iter().map(|d| agent_file(plugin, d)).filter(|f| !dir.join(f).exists()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} agent file(s) present", agents.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("agent file(s) missing: {}", missing.join(", ")),
                fix: "run the host's `setup` in the project root".into(),
            },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/vscode_copilot.rs"]
mod vscode_copilot_tests;
