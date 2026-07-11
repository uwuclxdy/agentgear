//! The agent backend seam. `reconcile` is the one shape install/update/self_heal
//! all reduce to; each backend decides what "converged" means for its agent.
//!
//! The trait is **unsealed** as of the multi-harness work: the codex/opencode/json
//! adapters give the contract real evidence, so external + in-crate backends both
//! implement it. `probe` is the classification self_heal keys its marker table on.

use crate::doctor::DoctorReport;
use crate::error::Result;
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

#[cfg(feature = "claude")]
pub(crate) mod claude;

#[cfg(feature = "codex")]
pub(crate) mod codex;
// `allow(dead_code)`: the codex backend wires this renderer in pass B.
#[cfg(feature = "codex")]
#[allow(dead_code)]
pub(crate) mod mcptoml;

#[cfg(feature = "cline")]
pub(crate) mod cline;
#[cfg(feature = "cursor")]
pub(crate) mod cursor;
#[cfg(feature = "devin")]
pub(crate) mod devin;
#[cfg(feature = "gemini")]
pub(crate) mod gemini;
#[cfg(feature = "opencode")]
pub(crate) mod opencode;

// Shared config-writing helpers, compiled only when a non-CC backend needs them.
// `allow(dead_code)`: the calls land when the harness workflows fill their stubs
// (pass B); until then the renderer + editor sit unreferenced under `--all-features`.
#[cfg(any(feature = "opencode", feature = "gemini", feature = "cursor", feature = "cline", feature = "devin", feature = "codex"))]
#[allow(dead_code)]
pub(crate) mod confedit;
#[cfg(any(feature = "gemini", feature = "cursor", feature = "cline", feature = "devin"))]
#[allow(dead_code)]
pub(crate) mod mcpjson;

/// What `probe` classifies a plugin's per-agent state as. Drives self_heal's
/// marker × state table (never resurrect, never re-enable, repair drift).
pub enum BackendState {
    Absent,
    Healthy,
    Disabled,
    NeedsRepair,
}

pub trait AgentBackend {
    fn id(&self) -> &'static str;
    /// Is this agent installed on the host?
    fn detect(&self) -> bool;
    /// What this agent can host (plugins / mcp / hooks / scopes).
    fn capabilities(&self) -> Capabilities;
    /// Classify this plugin's current state for the agent (self_heal's input).
    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState>;
    /// Idempotent converge to `desired` at `scope`.
    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome>;
    /// Undo the install (does not touch the stamp marker; the caller owns that).
    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome>;
    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport;
}

/// Resolve a backend by id. Each non-CC arm is feature-gated so a default build
/// ships only Claude; `all-agents` (fixture + docker legs) lights every arm.
pub(crate) fn backend_for(id: &str) -> Option<Box<dyn AgentBackend>> {
    match id {
        #[cfg(feature = "claude")]
        "claude" => Some(Box::new(claude::ClaudeBackend)),
        #[cfg(feature = "codex")]
        "codex" => Some(Box::new(codex::CodexBackend)),
        #[cfg(feature = "opencode")]
        "opencode" => Some(Box::new(opencode::OpencodeBackend)),
        #[cfg(feature = "gemini")]
        "gemini" => Some(Box::new(gemini::GeminiBackend)),
        #[cfg(feature = "cursor")]
        "cursor" => Some(Box::new(cursor::CursorBackend)),
        #[cfg(feature = "cline")]
        "cline" => Some(Box::new(cline::ClineBackend)),
        #[cfg(feature = "devin")]
        "devin" => Some(Box::new(devin::DevinBackend)),
        _ => None,
    }
}
