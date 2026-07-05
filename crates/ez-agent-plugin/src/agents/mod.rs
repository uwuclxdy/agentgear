//! The agent backend seam. `reconcile` is the one shape install/update/self_heal
//! all reduce to; each backend decides what "converged" means for its agent.
//!
//! The trait is **sealed in v1** (design §AgentBackend): one real impl means the
//! contract is still a guess, and freezing a guessed trait costs a semver major
//! the moment a second backend lands. It unseals when raawr's codex/opencode
//! adapters give the contract real evidence.

use crate::doctor::DoctorReport;
use crate::error::Result;
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

#[cfg(feature = "claude")]
pub(crate) mod claude;

mod sealed {
    pub trait Sealed {}
}

pub trait AgentBackend: sealed::Sealed {
    fn id(&self) -> &'static str;
    /// Is this agent installed on the host?
    fn detect(&self) -> bool;
    /// What this agent can host (plugins / mcp / hooks / scopes).
    fn capabilities(&self) -> Capabilities;
    /// Idempotent converge to `desired` at `scope`.
    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome>;
    /// Undo the install (does not touch the stamp marker; the caller owns that).
    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome>;
    fn report(&self, plugin: &Plugin, source: Source) -> DoctorReport;
}

/// Resolve a backend by id. v1 ships only the Claude backend; the match is where
/// codex/opencode land once their adapters exist.
pub(crate) fn backend_for(id: &str) -> Option<Box<dyn AgentBackend>> {
    match id {
        #[cfg(feature = "claude")]
        "claude" => Some(Box::new(claude::ClaudeBackend)),
        _ => None,
    }
}
