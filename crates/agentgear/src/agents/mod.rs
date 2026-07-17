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

#[cfg(feature = "amp")]
pub(crate) mod amp;
#[cfg(feature = "antigravity")]
pub(crate) mod antigravity;
#[cfg(feature = "antigravity-cli")]
pub(crate) mod antigravity_cli;
#[cfg(feature = "augment")]
pub(crate) mod augment;
#[cfg(feature = "cline")]
pub(crate) mod cline;
#[cfg(feature = "copilot-cli")]
pub(crate) mod copilot_cli;
#[cfg(feature = "crush")]
pub(crate) mod crush;
#[cfg(feature = "cursor")]
pub(crate) mod cursor;
#[cfg(feature = "devin")]
pub(crate) mod devin;
#[cfg(feature = "droid")]
pub(crate) mod droid;
#[cfg(feature = "gemini")]
pub(crate) mod gemini;
#[cfg(feature = "goose")]
pub(crate) mod goose;
#[cfg(feature = "jetbrains-copilot")]
pub(crate) mod jetbrains_copilot;
#[cfg(feature = "kilo")]
pub(crate) mod kilo;
#[cfg(feature = "kimi")]
pub(crate) mod kimi;
#[cfg(feature = "kiro")]
pub(crate) mod kiro;
#[cfg(feature = "omp")]
pub(crate) mod omp;
#[cfg(feature = "openclaw")]
pub(crate) mod openclaw;
#[cfg(feature = "opencode")]
pub(crate) mod opencode;
#[cfg(feature = "pi")]
pub(crate) mod pi;
#[cfg(feature = "qwen-code")]
pub(crate) mod qwen_code;
#[cfg(feature = "vscode-copilot")]
pub(crate) mod vscode_copilot;
#[cfg(feature = "zed")]
pub(crate) mod zed;

/// Every feature whose backend read-modify-writes a harness config file (all of
/// them except `claude`, which orchestrates the `claude plugin` CLI instead).
macro_rules! cfg_config_backends {
    ($item:item) => {
        #[cfg(any(
            feature = "codex",
            feature = "opencode",
            feature = "gemini",
            feature = "cursor",
            feature = "cline",
            feature = "devin",
            feature = "qwen-code",
            feature = "copilot-cli",
            feature = "vscode-copilot",
            feature = "jetbrains-copilot",
            feature = "kimi",
            feature = "kiro",
            feature = "zed",
            feature = "omp",
            feature = "openclaw",
            feature = "kilo",
            feature = "antigravity",
            feature = "antigravity-cli",
            feature = "pi",
            feature = "goose",
            feature = "amp",
            feature = "crush",
            feature = "droid",
            feature = "augment",
        ))]
        $item
    };
}

// Shared config-writing helpers, compiled only when a non-CC backend needs them.
// `allow(dead_code)`: not every enabled backend uses every helper, so a single-
// feature build leaves parts of the shared surface unreferenced.
cfg_config_backends! {
    #[allow(dead_code)]
    pub(crate) mod confedit;
}
cfg_config_backends! {
    #[allow(dead_code)]
    pub(crate) mod mcpjson;
}
cfg_config_backends! {
    #[allow(dead_code)]
    pub(crate) mod cchooks;
}
cfg_config_backends! {
    #[allow(dead_code)]
    pub(crate) mod report;
}

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
    /// `source` is the one self_heal already resolved for this agent (a rehydrated
    /// `--path`, else the compile-time default), so probe renders each surface from
    /// exactly the bytes `reconcile` would write — never a divergent embedded blob.
    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState>;
    /// Idempotent converge to `desired` at `scope`.
    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome>;
    /// Undo the install (does not touch the stamp marker; the caller owns that).
    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome>;
    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport;
}

/// Resolve a backend by id. Each non-CC arm is feature-gated so a default build
/// ships only Claude; `all-agents` (fixture + docker legs) lights every arm.
/// Public so a host can enumerate its `AGENTS` (`detect`/`capabilities`) to
/// build its own setup UI.
pub fn backend_for(id: &str) -> Option<Box<dyn AgentBackend>> {
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
        #[cfg(feature = "qwen-code")]
        "qwen-code" => Some(Box::new(qwen_code::QwenCodeBackend)),
        #[cfg(feature = "copilot-cli")]
        "copilot-cli" => Some(Box::new(copilot_cli::CopilotCliBackend)),
        #[cfg(feature = "vscode-copilot")]
        "vscode-copilot" => Some(Box::new(vscode_copilot::VscodeCopilotBackend)),
        #[cfg(feature = "jetbrains-copilot")]
        "jetbrains-copilot" => Some(Box::new(jetbrains_copilot::JetbrainsCopilotBackend)),
        #[cfg(feature = "kimi")]
        "kimi" => Some(Box::new(kimi::KimiBackend)),
        #[cfg(feature = "kiro")]
        "kiro" => Some(Box::new(kiro::KiroBackend)),
        #[cfg(feature = "zed")]
        "zed" => Some(Box::new(zed::ZedBackend)),
        #[cfg(feature = "omp")]
        "omp" => Some(Box::new(omp::OmpBackend)),
        #[cfg(feature = "openclaw")]
        "openclaw" => Some(Box::new(openclaw::OpenclawBackend)),
        #[cfg(feature = "kilo")]
        "kilo" => Some(Box::new(kilo::KiloBackend)),
        #[cfg(feature = "antigravity")]
        "antigravity" => Some(Box::new(antigravity::AntigravityBackend)),
        #[cfg(feature = "antigravity-cli")]
        "antigravity-cli" => Some(Box::new(antigravity_cli::AntigravityCliBackend)),
        #[cfg(feature = "pi")]
        "pi" => Some(Box::new(pi::PiBackend)),
        #[cfg(feature = "goose")]
        "goose" => Some(Box::new(goose::GooseBackend)),
        #[cfg(feature = "amp")]
        "amp" => Some(Box::new(amp::AmpBackend)),
        #[cfg(feature = "crush")]
        "crush" => Some(Box::new(crush::CrushBackend)),
        #[cfg(feature = "droid")]
        "droid" => Some(Box::new(droid::DroidBackend)),
        #[cfg(feature = "augment")]
        "augment" => Some(Box::new(augment::AugmentBackend)),
        _ => None,
    }
}
