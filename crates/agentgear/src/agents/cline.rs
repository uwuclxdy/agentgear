//! STUB: cline backend filled by its workflow. Cline hosts mcp under `mcpServers`
//! (`~/.cline/mcp.json` for the CLI; the config path is contested — see the brief)
//! with plain `{command, args, env}` bodies + file-config hooks (macOS/Linux only).
//! Uses `mcpjson` (ServerShape::Plain).

use super::{AgentBackend, BackendState};
use crate::doctor::DoctorReport;
use crate::error::Result;
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct ClineBackend;

impl AgentBackend for ClineBackend {
    fn id(&self) -> &'static str {
        "cline"
    }

    fn detect(&self) -> bool {
        which::which("cline").is_ok() || dirs::home_dir().is_some_and(|h| h.join(".cline").is_dir())
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
