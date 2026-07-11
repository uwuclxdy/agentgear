//! The kimi-code backend: a full translate into kimi's own config, rooted at
//! `~/.kimi-code/` (override `KIMI_CODE_HOME`). MCP goes through the shared json
//! renderer (`mcp.json` `mcpServers`, Plain shape — kimi's stdio body is
//! `{command,args,env}`); hooks land in `config.toml` as a `[[hooks]]`
//! array-of-tables under CC's exact event names (kimi mirrors them 1:1). Every mcp
//! key is our own server name and every hook is matched by its command string, so
//! `remove` is exact and a second reconcile is a true `NoOp`. Commands/agents/skills
//! have no user-level file surface here and are skipped (see `docs/harness/kimi.md`).
//!
//! Two products share the `kimi` binary — the legacy python `kimi-cli` (`~/.kimi`)
//! and the current TypeScript `kimi-code` (`~/.kimi-code`). This backend targets the
//! CURRENT one, so it detects on the `~/.kimi-code` config dir (not the ambiguous
//! binary name) and honors `KIMI_CODE_HOME` everywhere (see `docs/harness/kimi.md`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use super::confedit;
use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct KimiBackend;

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
        // No user-level command/agent file surface exists (kimi's slash-command analog
        // is a `SKILL.md` subdir, not a flat `.md`); mcp + hooks are the translated set.
        Capabilities { plugins: false, mcp: true, hooks: true, scopes: &["user"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        // Ownership is defined by our mcp server keys (the canonical "are we here"
        // signal); the shared probe returns Healthy — never Absent — for an mcp-less
        // plugin, so a present marker is never dropped. Source::Embedded is the only
        // steady-state source for a non-CC backend (github unsupported, path is
        // install-only), mirroring the claude probe keying on compile-time metadata.
        let comp = plugin.components(&Source::Embedded)?;
        mcpjson::probe(&mcp_json(scope)?, &["mcpServers"], &comp.mcp_servers, ServerShape::Plain)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let base = kimi_base(scope)?;

        let mut changed = false;
        changed |= mcpjson::reconcile(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, ServerShape::Plain)? != Outcome::NoOp;
        changed |= reconcile_hooks(&base.join("config.toml"), &comp.hooks)?;
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let base = kimi_base(scope)?;

        let mut changed = false;
        changed |= mcpjson::remove(&base.join("mcp.json"), &["mcpServers"], &portable_names(&comp.mcp_servers))? != Outcome::NoOp;
        changed |= remove_hooks(&base.join("config.toml"), &comp.hooks)?;
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

fn mcp_json(scope: &Scope) -> Result<PathBuf> {
    Ok(kimi_base(scope)?.join("mcp.json"))
}

/// Server names `reconcile` actually writes (the shared renderer skips non-portable
/// ones). `remove` keys off the same set so it never deletes a user server that
/// happens to share a name with one we declared but never wrote.
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
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

/// A `${CLAUDE_PLUGIN_ROOT}` reference only expands inside Claude Code, so a hook
/// carrying it would spawn the literal token under kimi. Mirrors
/// `McpServer::is_portable`; applied locally since `HookBinding` has no such method.
fn hook_is_portable(hook: &HookBinding) -> bool {
    !hook.command.contains("${CLAUDE_PLUGIN_ROOT}")
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

/// Strip exactly our `[[hooks]]` entries (matched by command string), leaving the
/// user's — including one under an event we also write to. A hook we never wrote (a
/// non-portable one, or a user's own) survives, since it never enters `ours`.
fn remove_hooks(config: &Path, hooks: &[HookBinding]) -> Result<bool> {
    if !config.exists() {
        return Ok(false);
    }
    let ours: BTreeSet<&str> = hooks.iter().filter(|h| hook_is_portable(h)).map(|h| h.command.as_str()).collect();
    confedit::toml_edit(config, |doc| {
        if let Some(arr) = doc.as_table_mut().get_mut("hooks").and_then(Item::as_array_of_tables_mut) {
            arr.retain(|t| t.get("command").and_then(Item::as_str).is_none_or(|c| !ours.contains(c)));
        }
        Ok(())
    })
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
    checks.push(check_hooks_present(&comp.hooks, &base.join("config.toml")));

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

/// Unlike codex, kimi hooks fire the moment they are written (no `/hooks` trust
/// gate), so a present hook is a plain `Ok`; a missing one we should have written is
/// a `Fail`. Substring scan of `config.toml` — reconcile already refused an
/// unparseable file, so a raw text check is enough to confirm presence.
fn check_hooks_present(hooks: &[HookBinding], config: &Path) -> DoctorCheck {
    let name = "translated hooks present";
    let ours: Vec<&str> =
        hooks.iter().filter(|h| hook_is_portable(h) && map_event(&h.event).is_some()).map(|h| h.command.as_str()).collect();
    if ours.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no hooks to translate".into()) };
    }
    let text = fs::read_to_string(config).unwrap_or_default();
    let missing: Vec<&str> = ours.iter().copied().filter(|c| !text.contains(c)).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} hook(s) present in config.toml", ours.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("hook(s) missing from config.toml: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/kimi.rs"]
mod kimi_tests;
