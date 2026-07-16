//! The "setup picker" host: a single derived struct declaring all 25 backend ids,
//! so a caller can enumerate `MultiInstaller::AGENTS` and target any subset with
//! `install_into` instead of installing into everything at once.
//!
//! The struct lives in the lib (not just `main.rs`) so `main.rs` and the tests
//! share one source of truth for the agent list instead of each hardcoding it.

use agentgear::PluginHost;

#[derive(PluginHost)]
#[plugin(name = "multi-installer", agents = [
    "claude", "codex", "opencode", "gemini", "cursor", "cline", "devin",
    "qwen-code", "copilot-cli", "vscode-copilot", "jetbrains-copilot",
    "kimi", "kiro", "zed", "omp", "openclaw", "kilo",
    "antigravity", "antigravity-cli", "pi",
    "goose", "amp", "crush", "droid", "augment",
])]
pub struct MultiInstaller;
