//! A canonical install / uninstall / self-heal lifecycle for the Claude Code
//! plugin a Rust binary ships, so a `setup` subcommand replaces the user typing
//! `/plugin marketplace add` + `/plugin install`.
//!
//! The lifecycle orchestrates the supported `claude plugin` CLI as its transaction
//! boundary; it never forges Claude Code's on-disk registry state. Design rationale
//! lives in `docs/design.md`.

#[cfg(feature = "derive")]
pub use ez_agent_plugin_derive::PluginHost;
