//! The minimal agentgear host: a single derived struct is the whole plugin.
//!
//! `#[derive(PluginHost)]` reads `plugin/.claude-plugin/plugin.json` at compile time,
//! bakes the tree into the binary (via the one-line `build.rs`), and implements the
//! lifecycle methods (`install`/`uninstall`/`doctor`/...). With no `agents = [...]`
//! attribute the host targets Claude Code only — the smallest real configuration.
//!
//! The struct lives in the lib (not just `main.rs`) so tests can assert the derived
//! metadata without spawning the binary.

use agentgear::PluginHost;

/// Host-authored always-loaded guidance. `#[plugin(instructions_fn = ...)]` names a
/// `fn() -> Option<String>` the derive calls from `PluginHost::instructions`; each
/// non-CC backend writes the returned text to its native context channel. hello-mcp
/// is claude-only (served this via MCP `instructions`, not a file), so nothing is
/// written here — the attr only demonstrates the descriptor wiring.
pub fn session_instructions() -> Option<String> {
    Some("hello from the minimal agentgear host".to_string())
}

#[derive(PluginHost)]
#[plugin(name = "hello-mcp", instructions_fn = session_instructions)]
pub struct HelloMcp;
