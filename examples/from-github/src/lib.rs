//! The zero-embed agentgear host: a binary that ships no baked plugin blob and
//! installs its plugin from a GitHub marketplace instead.
//!
//! Two things are off relative to the other examples:
//!
//! - `embed = false` on the derive: no `include_bytes!`, so [`FromGithub::embedded_blob`]
//!   is empty and the binary carries zero embed bytes. Paired with the lib's
//!   `default-features = false` (Cargo.toml) that also drops `tar`/`brotli`.
//! - `default_source = "github"`: [`FromGithub::DEFAULT_SOURCE`] is
//!   `Source::GitHub { repo, ref_ }`, so `update`/`self_heal`/`doctor` resolve
//!   against the GitHub tag rather than a baked tree. The `ref_` defaults to
//!   `v{CARGO_PKG_VERSION}` (the tag `claude plugin tag` produces), keeping the
//!   plugin version aligned with the binary.
//!
//! The local `plugin/` tree still exists and still matters: the derive reads its
//! `plugin.json` at compile time to cross-check the `name`, and the one-line
//! `build.rs` asserts `version` == `CARGO_PKG_VERSION`. That tree is the source you
//! would publish at the GitHub repo root; the binary installs whatever the repo
//! serves at the pinned ref.
//!
//! The struct lives in the lib so tests can read the derived metadata without
//! spawning the binary.

use agentgear::PluginHost;

#[derive(PluginHost)]
#[plugin(name = "from-github", embed = false, default_source = "github", github_repo = "uwuclxdy/agentgear", agents = ["claude"])]
pub struct FromGithub;
