//! STUB: codex backend filled by its workflow. Codex hosts mcp via toml
//! (`~/.codex/config.toml`, `[mcp_servers.<name>]`) + file-config hooks.

use super::{AgentBackend, BackendState};
use crate::doctor::DoctorReport;
use crate::error::Result;
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct CodexBackend;

impl AgentBackend for CodexBackend {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn detect(&self) -> bool {
        which::which("codex").is_ok() || dirs::home_dir().is_some_and(|h| h.join(".codex").is_dir())
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
