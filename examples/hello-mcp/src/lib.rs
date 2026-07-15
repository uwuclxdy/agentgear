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

#[derive(PluginHost)]
#[plugin(name = "hello-mcp")]
pub struct HelloMcp;
