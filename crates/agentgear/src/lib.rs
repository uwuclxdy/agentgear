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
//! The derive-driven example stays `ignore`: the macro reads a `plugin.json` tree
//! and the emitted guard needs `AGENTGEAR_GUARD`, neither of which a doctest has. A
//! compiled example of the derive-free value types follows below.
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
//! The value types need no derive, so this block is a real, compiled doctest. It
//! builds a [`Source`]/[`Scope`], then reads an [`Outcome`] and an [`AgentReport`]'s
//! per-agent results.
//!
//! ```
//! use agentgear::{AgentReport, AgentResult, AgentStatus, Outcome, Scope, Source};
//!
//! let _source = Source::Path("./plugin".into());
//! let _scope = Scope::Project { path: ".".into() };
//!
//! let outcome = Outcome::Updated { from: Some("0.1.0".into()), to: "0.2.0".into() };
//! let line = match outcome {
//!     Outcome::Installed => "installed".to_string(),
//!     Outcome::Updated { from, to } => format!("updated {from:?} -> {to}"),
//!     other => other.to_string(),
//! };
//! assert_eq!(line, "updated Some(\"0.1.0\") -> 0.2.0");
//!
//! let result = AgentResult { agent: "claude", status: AgentStatus::Converged(Outcome::Installed) };
//! assert!(matches!(result.status, AgentStatus::Converged(_)));
//!
//! // AgentReport is #[non_exhaustive]; only a lifecycle call builds one, so this
//! // reader is compile-checked against the live signatures without an instance.
//! fn summarize(report: &AgentReport) -> bool {
//!     for entry in &report.results {
//!         let _ = (entry.agent, &entry.status);
//!     }
//!     let _merged: Outcome = report.merged();
//!     report.is_healthy()
//! }
//! let _ = summarize as fn(&AgentReport) -> bool;
//! ```
//!
//! The host also authors a one-line `build.rs`:
//! `fn main() { agentgear::build::assert_plugin_version(); }`.
//!
//! Five runnable hosts live in the repo's `examples/`. `hello-mcp` is the minimal
//! Claude-only host; `kitchen-sink` exercises every component type across seven
//! harnesses; `multi-installer` builds an agent picker through [`backend_for`];
//! `hooks-everywhere` fans one hook event across many harnesses; `from-github` is a
//! zero-embed host tracking a GitHub source.
//!
//! # Feature flags
//!
//! `default = ["derive", "claude", "embed"]`: the [`PluginHost`] derive macro, the
//! Claude Code backend, and baking the plugin tree into the binary as a compressed
//! blob so `setup` works offline. Turning `embed` off (paired with `embed = false`
//! on the derive) ships a zero-embed binary for a host that installs from a GitHub
//! or path [`Source`] instead.
//!
//! Every other coding agent is its own feature, named by its backend id;
//! `all-agents` enables all 25 at once. Only four pull an extra dependency:
//!
//! | feature (= backend id) | extra dependency |
//! |---|---|
//! | `codex`, `kimi` | `toml_edit` (toml config) |
//! | `omp`, `goose` | `serde_norway` (yaml config) |
//! | `opencode`, `gemini`, `cursor`, `cline`, `devin`, `qwen-code`, `copilot-cli`, `vscode-copilot`, `jetbrains-copilot`, `kiro`, `zed`, `openclaw`, `kilo`, `antigravity`, `antigravity-cli`, `pi`, `amp`, `crush`, `droid`, `augment` | none |
//!
//! Backends are selected per host binary with the derive's `agents = [...]` list;
//! [`backend_for`] resolves an enabled id to its [`AgentBackend`], and [`AGENT_IDS`]
//! is the roster of ids compiled into the current build, so a host can drive its own
//! picker UI without hardcoding the list. The full set of 25 ids: plugin-native
//! `claude` and `copilot-cli` (full lifecycle through the tool's own CLI), plus the
//! 23 config-merge backends `codex`, `opencode`, `gemini`, `cursor`, `cline`,
//! `devin`, `qwen-code`, `vscode-copilot`, `jetbrains-copilot`, `kimi`, `kiro`,
//! `zed`, `omp`, `openclaw`, `kilo`, `antigravity`, `antigravity-cli`, `pi`, `goose`,
//! `amp`, `crush`, `droid`, `augment`. Design rationale and the full per-backend
//! reference live in the [project wiki](https://github.com/uwuclxdy/agentgear/wiki).

#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, doc(auto_cfg))]
#![warn(missing_docs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod agents;
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
mod cli;
mod components;
mod doctor;
mod error;
mod host;
mod install;
mod lock;
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
mod manifest;
mod materialize;
mod restart;
#[cfg(test)]
#[path = "../tests/unit/scratch.rs"]
mod scratch;
mod selfheal;
mod stamp;
mod util;

pub mod build;
pub mod statusline;
// The derive's compile-time listed-agent-vs-enabled-feature check reads these;
// hidden because nothing else should (the module doc has the full story).
#[doc(hidden)]
pub mod __feature_check;

pub use agents::{AGENT_IDS, AgentBackend, BackendState, backend_for};
pub use components::{AGENTGEAR_CLIENT_TOKEN, HookBinding, MarkdownDoc, McpKind, McpServer, PluginComponents, SkillDir};
pub use doctor::{CheckStatus, DoctorCheck, DoctorReport};
pub use error::{Error, Result};
pub use host::{
    AgentReport, AgentResult, AgentStatus, Capabilities, Desired, Outcome, Plugin, PluginHost, Scope, SkipReason, Source, current_pointer,
};
pub use statusline::StatusLineDecl;

#[cfg(feature = "derive")]
pub use agentgear_derive::PluginHost;

/// A ready-to-glob prelude for host binaries.
pub mod prelude {
    pub use crate::{AgentReport, DoctorReport, Outcome, PluginHost, Scope, Source};
}
