//! A canonical install / uninstall / self-heal lifecycle for the Claude Code
//! plugin a Rust binary ships, so a `setup` subcommand replaces the user typing
//! `/plugin marketplace add` + `/plugin install`.
//!
//! The lifecycle orchestrates the supported `claude plugin` CLI as its transaction
//! boundary; it never forges Claude Code's on-disk registry state. Design rationale
//! lives in `docs/design.md`.
//!
//! ```ignore
//! use agentgear::{PluginHost, Scope, Source};
//!
//! #[derive(PluginHost)]
//! #[plugin(name = "claudix", agents = ["claude"])]
//! struct ClaudixHost;
//!
//! ClaudixHost::install(Scope::User, Source::Embedded)?;
//! ClaudixHost::self_heal()?; // SessionStart entrypoint
//! ```
//!
//! The host also authors a one-line `build.rs`:
//! `fn main() { agentgear::build::assert_plugin_version(); }`.

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

pub use agents::{AgentBackend, BackendState};
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
