//! A canonical install / uninstall / self-heal lifecycle for the Claude Code
//! plugin a Rust binary ships, so a `setup` subcommand replaces the user typing
//! `/plugin marketplace add` + `/plugin install`.
//!
//! The lifecycle orchestrates the supported `claude plugin` CLI as its transaction
//! boundary; it never forges Claude Code's on-disk registry state. Design rationale
//! lives in `docs/design.md`.
//!
//! ```ignore
//! use ez_agent_plugin::{PluginHost, Scope, Source};
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
//! `fn main() { ez_agent_plugin::build::assert_plugin_version(); }`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod agents;
mod cli;
mod doctor;
mod error;
mod host;
mod install;
mod lock;
mod manifest;
mod materialize;
mod selfheal;
mod stamp;
mod util;

pub mod build;

pub use agents::AgentBackend;
pub use doctor::{CheckStatus, DoctorCheck, DoctorReport};
pub use error::{Error, Result};
pub use host::{Capabilities, Desired, Outcome, Plugin, PluginHost, Scope, Source};

#[cfg(feature = "derive")]
pub use ez_agent_plugin_derive::PluginHost;

/// Implementation detail wired up by the derive macro. Not a stable API.
#[doc(hidden)]
pub mod __private {
    // Aliased re-exports so the host stays on a single dependency: the
    // `include_dir!` expansion emits bare `include_dir::` paths, which the
    // generated `embedded_tree` brings into scope via `include_dir_crate`.
    pub use include_dir as include_dir_crate;
    pub use include_dir::Dir;
    pub use include_dir::include_dir as include_dir_macro;
}

/// A ready-to-glob prelude for host binaries.
pub mod prelude {
    pub use crate::{DoctorReport, Outcome, PluginHost, Scope, Source};
}
