//! STUB: gemini backend filled by its workflow. gemini-cli hosts mcp under
//! `mcpServers` (`~/.gemini/settings.json`, plain `{command, args, env}`) +
//! file-config hooks in the same settings file. Uses `mcpjson` (ServerShape::Plain).

use super::{AgentBackend, BackendState};
use crate::doctor::DoctorReport;
use crate::error::Result;
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct GeminiBackend;

impl AgentBackend for GeminiBackend {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn detect(&self) -> bool {
        which::which("gemini").is_ok() || dirs::home_dir().is_some_and(|h| h.join(".gemini").is_dir())
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
