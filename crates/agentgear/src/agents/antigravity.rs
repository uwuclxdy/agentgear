//! The Antigravity 2.0 desktop-IDE backend: an MCP-only translate into Antigravity's
//! shared config. MCP goes through the shared json renderer (Plain `{command,args,
//! env}`) into the `mcpServers` key of `~/.gemini/config/mcp_config.json` — the file
//! Antigravity 2.0's desktop IDE and the `agy` CLI both read. Writing through the
//! same shared renderer + key + shape + file makes this write byte-identical to the
//! `antigravity-cli` backend's, so installing both stays idempotent and their
//! removals are symmetric. Detection rides on the IDE-specific
//! `~/.gemini/antigravity-ide/` marker dir (the desktop app's own PATH binary name
//! is undocumented, and `agy` belongs to the CLI backend, not this one).
//!
//! Hooks, commands, subagents and skills all have no verified desktop-IDE file
//! surface and are skipped — the IDE's native events are workflow/git triggers, not
//! CC-style shell hooks, and its user-scope command surface is the skills dir that no
//! backend translates in v1. See `docs/harness/antigravity.md` for the full mapping.

use std::path::PathBuf;

use super::mcpjson::{self, RemoteShape, ServerShape};
use super::report;
use super::{AgentBackend, BackendState};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct AntigravityBackend;

/// The shared antigravity mcp schema (`additionalProperties:false`) admits only
/// stdio (`command`) or SSE (`{serverUrl}`); a `type`/`url` key — or any http
/// server — voids the whole file, so http is skipped outright.
const SHAPE: ServerShape = ServerShape::plain().with_remote(RemoteShape::ServerUrlSseOnly);

impl AgentBackend for AntigravityBackend {
    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn detect(&self) -> bool {
        // Antigravity 2.0's desktop IDE and the `agy` CLI share `~/.gemini/config/`,
        // so that dir cannot tell them apart; `~/.gemini/antigravity-ide/` is the
        // IDE-specific marker (the app auto-generates per-tool MCP config under it).
        // HOME-based via `dirs`, so a test redirecting HOME redirects detection. No
        // binary probe: the desktop app's PATH binary name is undocumented, and `agy`
        // is the CLI backend's signal, never this one's.
        ide_marker().is_some_and(|p| p.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // MCP-only, user-scope-only: hooks/commands/agents/skills have no verified
        // desktop-IDE file surface (see the module doc + `docs/harness/antigravity.md`).
        Capabilities { plugins: false, mcp: true, hooks: false, commands: false, agents: false, skills: false, scopes: &["user"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Ownership is defined by our mcp server keys (the canonical "are we here"
        // signal); the shared probe returns Healthy — never Absent — for an mcp-less
        // plugin, so a present marker is never dropped. `source` is the one self_heal
        // resolved for this agent (rehydrated `--path`, else the compile-time default),
        // so probe and reconcile render identical bytes.
        let comp = plugin.components(source)?;
        let mcp = mcp_config(scope)?;
        mcpjson::probe(&mcp, &["mcpServers"], &comp.mcp_servers, SHAPE)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let mcp = mcp_config(scope)?;
        mcpjson::reconcile(&mcp, &["mcpServers"], &comp.mcp_servers, SHAPE)
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let mcp = mcp_config(scope)?;
        mcpjson::remove(&mcp, &["mcpServers"], &comp.mcp_servers, SHAPE)
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The shared Antigravity 2.0 config dir `~/.gemini/config` (desktop IDE + `agy` CLI
/// both read it). HOME-based via `dirs` with no documented env override, so a test
/// redirecting HOME redirects the path. Antigravity has no variable-expansion token
/// (it explicitly rejects `${workspaceFolder}`), so absolute paths are required —
/// enforced upstream by the `${CLAUDE_PLUGIN_ROOT}` portability filter that skips any
/// server carrying an unexpandable token.
fn gemini_config_dir() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".gemini").join("config"))
        .ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.gemini/config".into()))
}

/// The user-scope MCP config file. User-scope only (capabilities lists just
/// `"user"`); the orchestrator never reconciles a backend outside its declared
/// scopes, so the project arm is a guard — the IDE's project `.agents/` parity is
/// unconfirmed and deliberately not implemented.
fn mcp_config(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(gemini_config_dir()?.join("mcp_config.json")),
        Scope::Project { .. } => {
            Err(Error::Tree("antigravity backend is user-scope only (project-level `.agents/` IDE parity is unconfirmed)".into()))
        }
    }
}

/// The IDE-specific marker dir under `~/.gemini`. Distinct from the CLI's
/// `~/.gemini/antigravity-cli/`, so detection never mistakes one Antigravity for the
/// other.
fn ide_marker() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".gemini").join("antigravity-ide"))
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &AntigravityBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "antigravity detected", status: CheckStatus::Ok("~/.gemini/antigravity-ide present".into()) }
    } else {
        DoctorCheck {
            name: "antigravity detected",
            status: CheckStatus::Fail {
                problem: "Antigravity desktop IDE not detected".into(),
                fix: "install it from https://antigravity.google/download".into(),
            },
        }
    });

    let dir = match gemini_config_dir() {
        Ok(dir) => dir,
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp_config.json", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };
    let mcp = dir.join("mcp_config.json");

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

    checks
}

#[cfg(test)]
#[path = "../../tests/unit/antigravity.rs"]
mod antigravity_tests;
