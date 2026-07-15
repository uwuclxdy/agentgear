//! A canonical install / uninstall / self-heal lifecycle for the Claude Code
//! plugin a Rust binary ships, so a `setup` subcommand replaces the user typing
//! `/plugin marketplace add` + `/plugin install`. The same plugin tree can fan
//! out to 24 other coding agents (codex, gemini, opencode, cursor, goose, …),
//! each behind a cargo feature.
//!
//! For Claude Code the lifecycle orchestrates the `claude` CLI (≥ 2.1.196) as its
//! transaction boundary; it never forges Claude Code's on-disk registry state.
//! Non-Claude backends need no CLI at all: each translates the plugin's components
//! (MCP servers, hooks, commands, agents) into that tool's own config files via
//! atomic read-modify-write merges that leave the user's entries untouched, and
//! only runs when the tool is detected on the machine.
//!
//! ```ignore
//! use agentgear::{PluginHost, Scope, Source};
//!
//! #[derive(PluginHost)]
//! #[plugin(name = "claudix", agents = ["claude", "codex", "gemini"])]
//! struct ClaudixHost;
//!
//! ClaudixHost::install(Scope::User, Source::Embedded)?; // all detected agents
//! ClaudixHost::install_into(Scope::User, Source::Embedded, &["gemini"])?; // one
//! ClaudixHost::self_heal()?; // SessionStart entrypoint
//! ```
//!
//! The host also authors a one-line `build.rs`:
//! `fn main() { agentgear::build::assert_plugin_version(); }`.
//!
//! Two runnable hosts live in the repo's `examples/`: `hello-mcp` (the minimal
//! Claude-only host) and `kitchen-sink` (every component type across seven
//! harnesses, with hermetic lifecycle tests).
//!
//! # Feature flags
//!
//! - `derive`, `claude`, `embed` — the defaults: the [`PluginHost`] derive macro,
//!   the Claude Code backend, and baking the plugin tree into the binary as a
//!   compressed blob so `setup` works offline.
//! - one feature per non-Claude backend, named by its id (`codex`, `opencode`,
//!   `gemini`, `cursor`, `goose`, `crush`, …) — see `[features]` in Cargo.toml for
//!   the full list; `all-agents` turns on every one of them.
//! - disabling `embed` (with `embed = false` on the derive) ships a zero-embed
//!   binary for a host that installs from a GitHub or path [`Source`] instead.
//!
//! Backends are selected per host binary with the derive's `agents = [...]` list;
//! [`backend_for`] resolves an id to its [`AgentBackend`] when a host wants its
//! own picker UI. Design rationale lives in `docs/design.md`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod agents;
mod cli;
// The IR + parser is consumed by the non-CC backends + doctor in pass B;
// `allow(dead_code)` until those calls land.
#[allow(dead_code)]
mod components;
mod doctor;
mod error;
mod host;
mod install;
mod lock;
mod manifest;
mod materialize;
mod restart;
mod selfheal;
mod stamp;
mod util;

pub mod build;

pub use agents::{AgentBackend, BackendState, backend_for};
pub use components::{HookBinding, MarkdownDoc, McpKind, McpServer, PluginComponents, SkillDir};
pub use doctor::{CheckStatus, DoctorCheck, DoctorReport};
pub use error::{Error, Result};
pub use host::{Capabilities, Desired, Outcome, Plugin, PluginHost, Scope, Source};

#[cfg(feature = "derive")]
pub use agentgear_derive::PluginHost;

/// A ready-to-glob prelude for host binaries.
pub mod prelude {
    pub use crate::{DoctorReport, Outcome, PluginHost, Scope, Source};
}
