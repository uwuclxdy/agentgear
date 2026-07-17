//! The antigravity-cli (`agy`) backend: a translate into Antigravity's own file
//! config. MCP goes through the shared json renderer under the SHARED
//! `~/.gemini/config/mcp_config.json` `mcpServers` key, `ServerShape::plain()`
//! (`{command,args,env}`) — byte-identical to the antigravity desktop backend
//! (same file, same key, same renderer), so a double-install across the two
//! antigravity backends is a true `NoOp`. Hooks land in the same customization
//! root (`~/.gemini/config/hooks.json`), keyed by our plugin name at the top
//! level (`{"<plugin>":{"<Event>":[...]}}`), so we own that whole subtree and
//! `remove` is an exact single-key delete. Commands/agents/skills/rules are
//! skipped — MCP is the only surface backed by an official-Google source, so the
//! rest is left out per the research brief (see `docs/harness/antigravity-cli.md`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::cchooks::hook_is_portable;
use super::confedit::json_edit;
use super::mcpjson::{self, RemoteShape, ServerShape};
use super::report;
use super::{AgentBackend, BackendState};
use crate::components::HookBinding;
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct AntigravityCliBackend;

/// The shared antigravity mcp schema (`additionalProperties:false`) admits only
/// stdio (`command`) or SSE (`{serverUrl}`); a `type`/`url` key — or any http
/// server — voids the whole file, so http is skipped outright.
const SHAPE: ServerShape = ServerShape::plain().with_remote(RemoteShape::ServerUrlSseOnly);

impl AgentBackend for AntigravityCliBackend {
    fn id(&self) -> &'static str {
        "antigravity-cli"
    }

    fn detect(&self) -> bool {
        // `agy` on PATH is the direct signal; `~/.gemini/antigravity-cli/` is the
        // CLI-specific config dir (distinct from the shared `~/.gemini/config/`),
        // so a test redirecting `HOME` redirects detection. `ANTIGRAVITY_API_KEY`
        // is an auth INPUT a user may export without the CLI installed, so it is
        // deliberately NOT a detection signal.
        which::which("agy").is_ok() || dirs::home_dir().is_some_and(|h| h.join(".gemini").join("antigravity-cli").is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { plugins: false, mcp: true, hooks: true, scopes: &["user", "project"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        // Compose every surface this backend writes (mcp + the plugin-keyed hook
        // subtree), so a broken hook tree behind a healthy mcp entry reads as
        // NeedsRepair and a partial deletion never collapses to Absent (dropping the
        // marker, orphaning the surviving surface). Source::Embedded is the only
        // steady-state source for a non-CC backend (github unsupported, path install-only).
        let comp = plugin.components(&Source::Embedded)?;
        let mcp = mcpjson::probe_surface(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, SHAPE)?;
        // phase 3 (§6): enabled:false -> Disabled here
        let hooks = report::probe_json_subtree(&hooks_path(scope)?, &[plugin.name], render_hook_tree(&comp.hooks))?;
        Ok(report::compose([mcp, hooks].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;

        let mut changed = false;
        changed |= mcpjson::reconcile(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, SHAPE)? != Outcome::NoOp;
        changed |= reconcile_hooks(&hooks_path(scope)?, plugin.name, &comp.hooks)?;
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;

        let mut changed = false;
        changed |= mcpjson::remove(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, SHAPE)? != Outcome::NoOp;
        changed |= remove_hooks(&hooks_path(scope)?, plugin.name)?;
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// `~/.gemini` — the tree Antigravity 2.0 (desktop IDE + CLI) share. User scope
/// needs `HOME`; a missing home is a clear, actionable error.
fn gemini_home() -> Result<PathBuf> {
    dirs::home_dir().map(|h| h.join(".gemini")).ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.gemini".into()))
}

/// The MCP config: user scope = the SHARED `~/.gemini/config/mcp_config.json`
/// (read by both the desktop IDE and `agy`); project scope = Antigravity's native
/// per-workspace `<root>/.agents/mcp_config.json` (best-effort — the CLI supports
/// it but IDE parity is contested, see the brief).
fn mcp_path(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(gemini_home()?.join("config").join("mcp_config.json")),
        Scope::Project { path } => Ok(path.join(".agents").join("mcp_config.json")),
    }
}

/// The hooks config, always in the scope's customization root: user scope = the
/// shared `~/.gemini/config/hooks.json`; project scope = `<root>/.agents/hooks.json`
/// (loads only after the folder is trusted). `~/.gemini/antigravity-cli/` holds the
/// CLI's own settings and transcripts, is a customization root for nothing, and a
/// `hooks.json` written there is never loaded (Google's CHANGELOG v1.0.8 records
/// fixing this exact bug in their own `/hooks` command).
fn hooks_path(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(gemini_home()?.join("config").join("hooks.json")),
        Scope::Project { path } => Ok(path.join(".agents").join("hooks.json")),
    }
}

// --- hooks -------------------------------------------------------------------

/// Map a CC hook event onto one of `agy`'s five legal events: `PreToolUse`,
/// `PostToolUse`, `PreInvocation`, `PostInvocation`, `Stop`. Anything outside that
/// set is accepted by the file and silently never fires, which is worse than a skip
/// (it reads as wired), so an event with no analog is dropped. `PreInvocation` runs
/// before the model does, making it CC's `UserPromptSubmit` analog. `SessionStart`
/// has none: `agy` carries no session-level hook at all.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "UserPromptSubmit" => Some("PreInvocation"),
        "PreToolUse" => Some("PreToolUse"),
        "PostToolUse" => Some("PostToolUse"),
        "Stop" => Some("Stop"),
        _ => None,
    }
}

/// `agy` reads its two tool events as `{matcher, hooks:[handler,…]}` groups and
/// every other event as a flat handler list. One event, one nesting: a flat handler
/// under `PreToolUse` puts the command where nothing reads it.
fn is_grouped(agy_event: &str) -> bool {
    matches!(agy_event, "PreToolUse" | "PostToolUse")
}

/// One `agy` hook handler: `{type:"command", command}`. `timeout` is omitted (the
/// components IR carries none and `agy` defaults it to 30s), and `matcher` never
/// belongs here: a flat event has no matcher at all, and a grouped one carries it
/// on the wrapper.
fn render_handler(hook: &HookBinding) -> Value {
    let mut handler = Map::new();
    handler.insert("type".into(), Value::from("command"));
    handler.insert("command".into(), Value::from(hook.command.clone()));
    Value::Object(handler)
}

/// The handler list for one event, in that event's own nesting. Flat events take the
/// handlers directly. Grouped events take one `{matcher, hooks:[…]}` per matcher, so
/// handlers sharing a matcher stack inside one group rather than repeating it.
///
/// A CC hook with no matcher means "every tool", which the grouped shape can only say
/// through the matcher field; `*` is the value `agy`'s own `PostToolUse` doc example
/// uses. Its wildcard semantics are not separately proven, and an omitted `matcher`
/// key's meaning is not proven either, so the documented spelling wins. A matcher the
/// CC plugin *did* set is passed through verbatim and will not match: CC tool names
/// (`Bash`) are not `agy` tool names (`run_command`). Same accepted limit as gemini.
fn render_event(agy_event: &str, hooks: &[&HookBinding]) -> Value {
    if !is_grouped(agy_event) {
        return Value::Array(hooks.iter().map(|h| render_handler(h)).collect());
    }
    let mut groups: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for hook in hooks {
        groups.entry(hook.matcher.as_deref().unwrap_or("*")).or_default().push(render_handler(hook));
    }
    Value::Array(
        groups
            .into_iter()
            .map(|(matcher, handlers)| {
                let mut group = Map::new();
                group.insert("matcher".into(), Value::from(matcher));
                group.insert("hooks".into(), Value::Array(handlers));
                Value::Object(group)
            })
            .collect(),
    )
}

/// Our whole hook subtree (`{"<Event>":[entry,...]}`) — the value that lands under
/// the top-level `<plugin>` key. `None` when nothing survives (all non-portable, or
/// no Antigravity analog), so no empty subtree is ever written. `probe` and
/// `reconcile` both build it here, so they stay in lockstep.
fn render_hook_tree(hooks: &[HookBinding]) -> Option<Value> {
    let writable: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|event| (event, h))).collect();
    if writable.is_empty() {
        return None;
    }
    let mut by_event: BTreeMap<&'static str, Vec<&HookBinding>> = BTreeMap::new();
    for (event, hook) in writable {
        by_event.entry(event).or_default().push(hook);
    }
    let mut events: Map<String, Value> = Map::new();
    for (event, group) in &by_event {
        events.insert((*event).to_string(), render_event(event, group));
    }
    Some(Value::Object(events))
}

/// Write our whole hook subtree under the top-level `<plugin>` key
/// (`{"<plugin>":{"<Event>":[entry,...]}}`). We own that key, so a wholesale set
/// is exact and idempotent: `json_edit` skips the write when the rebuilt subtree
/// deep-equals the existing one. When nothing survives, `json_edit` is not entered
/// so no empty `hooks.json` is created.
fn reconcile_hooks(path: &Path, plugin: &str, hooks: &[HookBinding]) -> Result<bool> {
    let Some(tree) = render_hook_tree(hooks) else {
        return Ok(false);
    };
    json_edit(path, |root| {
        if let Value::Object(map) = root {
            map.insert(plugin.to_string(), tree);
        }
        Ok(())
    })
}

/// Drop exactly our top-level `<plugin>` key, leaving every other plugin's (or the
/// user's own) top-level hook entry untouched. Since we only ever wrote portable
/// hooks under our own key, a whole-key delete can never reach a foreign entry —
/// no per-command portability filter is needed here.
fn remove_hooks(path: &Path, plugin: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    json_edit(path, |root| {
        if let Value::Object(map) = root {
            map.remove(plugin);
        }
        Ok(())
    })
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &AntigravityCliBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck {
            name: "antigravity-cli detected",
            status: CheckStatus::Ok("`agy` on PATH or ~/.gemini/antigravity-cli present".into()),
        }
    } else {
        DoctorCheck {
            name: "antigravity-cli detected",
            status: CheckStatus::Fail {
                problem: "antigravity-cli (`agy`) not detected".into(),
                fix: "install it with `curl -fsSL https://antigravity.google/cli/install.sh | bash`".into(),
            },
        }
    });

    let mcp = match mcp_path(&Scope::User) {
        Ok(mcp) => mcp,
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp_config.json", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };

    let root = report::read_json_config(&mut checks, "mcp_config.json", &mcp);

    let Some(comp) = report::components(&mut checks, plugin, source) else {
        return checks;
    };

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcpServers"],
        "not in mcp_config.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    checks.push(check_hooks_registered(plugin.name, &comp.hooks));

    checks
}

fn check_hooks_registered(plugin: &str, hooks: &[HookBinding]) -> DoctorCheck {
    let name = "hooks registered";
    let writable = hooks.iter().filter(|h| hook_is_portable(h)).any(|h| map_event(&h.event).is_some());
    if !writable {
        return DoctorCheck { name, status: CheckStatus::Ok("no portable, mappable hooks to register".into()) };
    }
    let path = match hooks_path(&Scope::User) {
        Ok(p) => p,
        Err(e) => return DoctorCheck { name, status: CheckStatus::Warn(e.to_string()) },
    };
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) if v.get(plugin).is_some() => DoctorCheck { name, status: CheckStatus::Ok(format!("{plugin} hook entry present")) },
            Ok(_) => DoctorCheck {
                name,
                status: CheckStatus::Fail {
                    problem: format!("no `{plugin}` entry in {}", path.display()),
                    fix: "run the host's `setup`".into(),
                },
            },
            Err(e) => DoctorCheck {
                name,
                status: CheckStatus::Fail {
                    problem: format!("{} does not parse: {e}", path.display()),
                    fix: "fix the JSON syntax or remove the file".into(),
                },
            },
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            DoctorCheck { name, status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", path.display())) }
        }
        Err(e) => DoctorCheck { name, status: CheckStatus::Warn(format!("could not read {}: {e}", path.display())) },
    }
}

#[cfg(test)]
#[path = "../../tests/unit/antigravity_cli.rs"]
mod antigravity_cli_tests;
