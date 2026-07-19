//! One `bool` const per agent id, `true` iff that backend's cargo feature is
//! compiled in. The derive emits a const-eval panic against these in the HOST
//! crate (same trick as `AGENTGEAR_GUARD`), so `agents = ["codex"]` without
//! `features = ["codex"]` is a compile error with the fix instead of a runtime
//! "no backend for agent" on the end user's machine. Not part of the public API.
//!
//! A unit `const X: () = panic!()` cannot live here instead: a const item is
//! evaluated eagerly in its defining crate, so it would fail agentgear's own
//! build whenever the feature is off (verified 2026-07-19). The bool + a
//! derive-emitted `if !X { panic!() }` moves the evaluation into the host crate,
//! where the message can name the missing feature.
//!
//! The id list here, the `backend_for` registry, and the derive's
//! `KNOWN_AGENTS` are pinned together by the derive crate's
//! `known_agents_match_the_lib` test.

macro_rules! feature_consts {
    ($($feature:literal => $ident:ident),* $(,)?) => {
        $(pub const $ident: bool = cfg!(feature = $feature);)*
    };
}

feature_consts! {
    "claude" => CLAUDE,
    "codex" => CODEX,
    "opencode" => OPENCODE,
    "gemini" => GEMINI,
    "cursor" => CURSOR,
    "cline" => CLINE,
    "devin" => DEVIN,
    "qwen-code" => QWEN_CODE,
    "copilot-cli" => COPILOT_CLI,
    "vscode-copilot" => VSCODE_COPILOT,
    "jetbrains-copilot" => JETBRAINS_COPILOT,
    "kimi" => KIMI,
    "kiro" => KIRO,
    "zed" => ZED,
    "omp" => OMP,
    "openclaw" => OPENCLAW,
    "kilo" => KILO,
    "antigravity" => ANTIGRAVITY,
    "antigravity-cli" => ANTIGRAVITY_CLI,
    "pi" => PI,
    "goose" => GOOSE,
    "amp" => AMP,
    "crush" => CRUSH,
    "droid" => DROID,
    "augment" => AUGMENT,
}
