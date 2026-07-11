//! STUB: opencode backend filled by its workflow. opencode hosts mcp under a
//! top-level `mcp` object (`~/.config/opencode/opencode.json`, `{type:"local",
//! command:[cmd, ...args], enabled, environment}`) via `confedit::json_edit`. Its
//! hooks are TS-plugin-only, not file config, so hooks are not translatable.

use super::{AgentBackend, BackendState};
use crate::doctor::DoctorReport;
use crate::error::Result;
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct OpencodeBackend;

impl AgentBackend for OpencodeBackend {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn detect(&self) -> bool {
        which::which("opencode").is_ok() || dirs::config_dir().is_some_and(|c| c.join("opencode").is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { plugins: false, mcp: true, hooks: false, scopes: &["user", "project"] }
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
