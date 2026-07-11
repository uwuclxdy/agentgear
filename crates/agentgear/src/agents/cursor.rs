//! STUB: cursor backend filled by its workflow. Cursor (editor) hosts mcp under
//! `mcpServers` (typed `{type:"stdio", command, args, env}`) in `~/.cursor/mcp.json`,
//! plus file-config hooks in `~/.cursor/hooks.json`; renders via `mcpjson`
//! (`ServerShape::Typed`). GUI-only, so detected by config-dir presence.

use super::{AgentBackend, BackendState};
use crate::doctor::DoctorReport;
use crate::error::Result;
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct CursorBackend;

impl AgentBackend for CursorBackend {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn detect(&self) -> bool {
        // The editor has no headless probe; `~/.cursor/` is the only host-agnostic
        // "configured here" signal. `cursor-agent` (the optional CLI) is a bonus.
        dirs::home_dir().is_some_and(|h| h.join(".cursor").is_dir()) || which::which("cursor-agent").is_ok()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { plugins: false, mcp: true, hooks: true, scopes: &["user", "project"] }
    }

    fn probe(&self, _plugin: &Plugin, _scope: &Scope) -> Result<BackendState> {
        Ok(BackendState::Absent)
    }

    fn reconcile(&self, _plugin: &Plugin, _desired: &Desired, _scope: &Scope) -> Result<Outcome> {
        Ok(Outcome::NoOp)
    }

    fn remove(&self, _plugin: &Plugin, _scope: &Scope) -> Result<Outcome> {
        Ok(Outcome::NoOp)
    }

    fn report(&self, _plugin: &Plugin, _source: &Source) -> DoctorReport {
        DoctorReport::from_checks(Vec::new())
    }
}
