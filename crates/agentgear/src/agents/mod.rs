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
#[cfg(feature = "codex")]
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

/// Every feature whose backend renders the plugin into a harness's own config files:
/// the roster minus the two plugin-native backends (`claude`, `copilot-cli`, which
/// orchestrate their tool's own plugin CLI) and `pi` (detect-only — it writes no file
/// and reads no component, so it needs none of the shared renderers below).
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
// feature build leaves parts of the shared surface unreferenced. A backend that
// writes a config file and lands in neither set loses `confedit` entirely and reds
// with `E0432` that no `--all-features` gate leg can see. A backend that writes
// none (`pi`) belongs in neither set and compiles clean without it.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
#[allow(dead_code)]
pub(crate) mod confedit;
cfg_config_backends! {
    #[cfg(not(any(feature = "claude", feature = "copilot-cli")))]
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
cfg_config_backends! {
    #[allow(dead_code)]
    pub(crate) mod skillsdir;
}

// The HOME-based Claude Code registry reader, shared by every backend that
// detects native CC-plugin-tree ingestion already covering it (omp's
// `claude-plugins` provider, cursor's default-on `loadClaude` loader).
#[cfg(any(feature = "omp", feature = "cursor"))]
pub(crate) mod ccregistry;

/// What `probe` classifies a plugin's per-agent state as. Drives self_heal's
/// marker × state table (never resurrect, never re-enable, repair drift).
pub enum BackendState {
    /// The plugin is not present for this agent.
    Absent,
    /// Present and converged to the desired state.
    Healthy,
    /// Present but deliberately disabled; self_heal leaves it alone.
    Disabled,
    /// Present but drifted from what `reconcile` would write.
    NeedsRepair,
}

/// One coding agent's translation of the plugin. `reconcile` is the single shape
/// install/update/self_heal reduce to; each backend decides what "converged" means
/// for its harness. The trait is unsealed, so an out-of-crate crate can add a
/// backend and drive it via [`Plugin::components`](crate::Plugin::components) plus a
/// direct `reconcile`/`remove` (the derive's `agents` list only names built-in ids).
pub trait AgentBackend {
    /// The backend's stable id, matching its cargo feature name.
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
    /// Undo the install, stripping exactly what a reconcile from `source` would
    /// have written — the caller resolves `source` per agent (a `--path`
    /// install's marker-rehydrated tree, else the compile-time default), so a
    /// path-installed agent's entries are removed by rendering the SAME tree
    /// they came from, never a divergent embedded blob. Plugin-native backends
    /// drive their own CLI for removal and ignore it. Does not touch the stamp
    /// marker; the caller owns that.
    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome>;
    /// This agent's slice of `doctor`: one check per surface it manages.
    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport;
}

/// Every agent id compiled into this build, in the order [`backend_for`] resolves
/// them: one entry per enabled backend feature. A host enumerates these to know
/// which backends it can target without hardcoding the roster — every id here
/// resolves to `Some` through [`backend_for`], pinned by a test. Feature-gated, so a
/// default build holds only `["claude"]` and `all-agents` holds all 25.
pub const AGENT_IDS: &[&str] = &[
    #[cfg(feature = "claude")]
    "claude",
    #[cfg(feature = "codex")]
    "codex",
    #[cfg(feature = "opencode")]
    "opencode",
    #[cfg(feature = "gemini")]
    "gemini",
    #[cfg(feature = "cursor")]
    "cursor",
    #[cfg(feature = "cline")]
    "cline",
    #[cfg(feature = "devin")]
    "devin",
    #[cfg(feature = "qwen-code")]
    "qwen-code",
    #[cfg(feature = "copilot-cli")]
    "copilot-cli",
    #[cfg(feature = "vscode-copilot")]
    "vscode-copilot",
    #[cfg(feature = "jetbrains-copilot")]
    "jetbrains-copilot",
    #[cfg(feature = "kimi")]
    "kimi",
    #[cfg(feature = "kiro")]
    "kiro",
    #[cfg(feature = "zed")]
    "zed",
    #[cfg(feature = "omp")]
    "omp",
    #[cfg(feature = "openclaw")]
    "openclaw",
    #[cfg(feature = "kilo")]
    "kilo",
    #[cfg(feature = "antigravity")]
    "antigravity",
    #[cfg(feature = "antigravity-cli")]
    "antigravity-cli",
    #[cfg(feature = "pi")]
    "pi",
    #[cfg(feature = "goose")]
    "goose",
    #[cfg(feature = "amp")]
    "amp",
    #[cfg(feature = "crush")]
    "crush",
    #[cfg(feature = "droid")]
    "droid",
    #[cfg(feature = "augment")]
    "augment",
];

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
