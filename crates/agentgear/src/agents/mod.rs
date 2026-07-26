//! The agent backend seam. `reconcile` is the one shape install/update/self_heal
//! all reduce to; each backend decides what "converged" means for its agent.
//!
//! The trait is **unsealed** as of the multi-harness work: the codex/opencode/json
//! adapters give the contract real evidence, so external + in-crate backends both
//! implement it. `probe` is the classification self_heal keys its marker table on.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::doctor::DoctorReport;
use crate::error::{Error, Result};
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
pub(crate) use cfg_config_backends;

// Shared config-writing helpers, compiled only when a non-CC backend needs them.
// `allow(dead_code)`: not every enabled backend uses every helper, so a single-
// feature build leaves parts of the shared surface unreferenced.
// `confedit` is the one shared helper the two PLUGIN-NATIVE backends need too: a
// host-owned status-line slot lives in the user's own settings file (CC's
// `settings.json`, copilot's `$COPILOT_HOME/settings.json`), so each of those
// backends read-modify-writes exactly one config file on top of its CLI
// orchestration. The real gate is therefore "the config-backend set PLUS the
// plugin-native slot backends" — split into two declarations because the shared macro
// carries only the non-CC set, and pulling those two into the macro would drag the
// other four helpers into every default build. Widen BOTH arms when a third
// plugin-native backend gains a slot: a backend that writes a config file and lands
// in neither set loses `confedit` entirely and reds with `E0432` that no
// `--all-features` gate leg can see. A backend that writes none (`pi`) belongs in
// neither set and compiles clean without it.
/// Reject an empty config-dir env override (`CLAUDE_CONFIG_DIR`, `COPILOT_HOME`)
/// instead of silently falling back to the default: both CLIs join their config
/// paths onto the value literally, so an empty override resolves to the current
/// directory rather than behaving as unset. Shared by `claude::cc_config_dir` and
/// `copilot_cli::copilot_home`, the two backends this was proven on.
///
/// Pure so the unset/empty/set decision is unit-testable without mutating process
/// env (`std::env::set_var` is `unsafe` and racy across threads in edition 2024);
/// [`config_dir_override`] below does the actual lookup.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
pub(crate) fn non_empty_config_dir(var: &'static str, value: Option<OsString>) -> Result<Option<PathBuf>> {
    match value {
        None => Ok(None),
        Some(v) if v.is_empty() => Err(Error::EmptyConfigDirOverride { var }),
        Some(v) => Ok(Some(PathBuf::from(v))),
    }
}

/// [`non_empty_config_dir`] wired to the real `var` lookup.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
pub(crate) fn config_dir_override(var: &'static str) -> Result<Option<PathBuf>> {
    non_empty_config_dir(var, std::env::var_os(var))
}

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
#[allow(dead_code)]
pub(crate) mod confedit;
cfg_config_backends! {
    #[cfg(not(any(feature = "claude", feature = "copilot-cli")))]
    #[allow(dead_code)]
    pub(crate) mod confedit;
}
/// Every feature whose backend writes a host-owned status-line slot
/// (`Capabilities::statusline`) — plugin-native and config-merge alike, which is why
/// this is its own set rather than either family's. One place to extend per backend.
macro_rules! cfg_statusline_backends {
    ($item:item) => {
        #[cfg(any(feature = "claude", feature = "qwen-code", feature = "antigravity-cli", feature = "droid", feature = "copilot-cli"))]
        $item
    };
}
pub(crate) use cfg_statusline_backends;

cfg_statusline_backends! {
    // `allow(dead_code)`: the shape axes are per-harness, so any single-backend build
    // leaves the ones it does not use unreferenced (same idiom as `mcpjson` above).
    #[allow(dead_code)]
    pub(crate) mod statuslinejson;
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
    /// Undo any write this backend made OUTSIDE the harness's own registry or plugin
    /// tree, before the stamp marker is cleared.
    ///
    /// Implement this whenever a backend writes into a file the USER owns and the
    /// harness does not carry — CC's `statusLine` slot in `settings.json` is the first,
    /// and every further status-line backend is the same shape. Such a write breaks the
    /// premise the teardown paths were built on: *an absent or already-removed tool has
    /// nothing of ours left behind.* It does not, because the user's settings file
    /// outlives the tool's registry, its plugin tree, and the tool binary itself. Every
    /// short-circuit on the way to `stamp::clear` — an undetected harness, an
    /// unsupported scope or source, a plugin the user removed by hand — therefore
    /// strands our value in their file and drops the marker that is the only copy of
    /// what we displaced.
    ///
    /// Both teardown paths (`install::uninstall_agent`, `selfheal`'s
    /// plugin-already-gone row) call this on every branch that reaches the marker
    /// clear, including the skips, and propagate its error so a failed restore keeps
    /// the marker rather than clearing the last copy of the user's data. Neither rests
    /// on any backend's current `Capabilities` or `detect()` answer. The one branch
    /// that skips it is a failed `remove`, which returns before the clear, so the
    /// marker survives there too.
    ///
    /// The default does nothing, which stays right for every config-merge backend:
    /// their writes ARE the plugin's translation, so tearing those down is `remove`'s
    /// job, and the plugin-already-gone row must keep only forgetting a marker (never
    /// resurrect, never delete on the user's behalf).
    fn forget(&self, _plugin: &Plugin, _scope: &Scope) -> Result<()> {
        Ok(())
    }
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

#[cfg(all(test, any(feature = "claude", feature = "copilot-cli")))]
mod config_dir_tests {
    use super::non_empty_config_dir;
    use crate::error::Error;

    #[test]
    fn unset_resolves_to_none() {
        assert!(matches!(non_empty_config_dir("TEST_VAR", None), Ok(None)));
    }

    #[test]
    fn non_empty_resolves_to_the_path() {
        let resolved = non_empty_config_dir("TEST_VAR", Some("/some/dir".into())).unwrap();
        assert_eq!(resolved, Some(std::path::PathBuf::from("/some/dir")));
    }

    #[test]
    fn empty_is_rejected_naming_the_variable() {
        let err = non_empty_config_dir("TEST_VAR", Some("".into())).unwrap_err();
        assert!(matches!(err, Error::EmptyConfigDirOverride { var: "TEST_VAR" }), "wrong variant: {err:?}");
    }

    // Non-UTF8 values are a real `var_os` result (an env var set via raw bytes), so the
    // resolver must not assume valid UTF-8 anywhere on the non-empty path.
    #[cfg(unix)]
    #[test]
    fn non_utf8_value_still_resolves() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let raw = OsString::from_vec(vec![0x66, 0x6f, 0xff, 0x6f]); // "fo\xFFo"
        let resolved = non_empty_config_dir("TEST_VAR", Some(raw.clone())).unwrap();
        assert_eq!(resolved, Some(std::path::PathBuf::from(raw)));
    }
}
