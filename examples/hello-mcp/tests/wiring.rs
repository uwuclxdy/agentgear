//! Regression coverage for the minimal host. hello-mcp targets Claude Code only,
//! and the real `claude` CLI is the transaction boundary its install drives — so a
//! plain `cargo test` has no hermetic install path to exercise (that lives in the
//! crate's `--ignored` e2e suite). What this file locks down is the wiring the
//! `#[derive(PluginHost)]` + one-line `build.rs` produce: if the derive, the embed,
//! or the plugin.json/CARGO_PKG_VERSION pin ever silently broke, these fail.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agentgear::PluginHost;
use hello_mcp::HelloMcp;

#[test]
fn derive_metadata_is_wired() {
    // No `agents = [...]` attr on the derive -> defaults to Claude Code only.
    assert_eq!(HelloMcp::NAME, "hello-mcp");
    assert_eq!(HelloMcp::MARKETPLACE, "hello-mcp");
    assert_eq!(HelloMcp::AGENTS, &["claude"]);
    // build.rs pins plugin.json `version` to this, so the const equals the crate version.
    assert_eq!(HelloMcp::VERSION, env!("CARGO_PKG_VERSION"));

    let descriptor = HelloMcp::descriptor();
    assert_eq!(descriptor.name, "hello-mcp");
    assert_eq!(descriptor.id(), "hello-mcp@hello-mcp");
    assert_eq!(descriptor.version, env!("CARGO_PKG_VERSION"));
    // `#[plugin(instructions_fn = session_instructions)]` splices the override into the
    // derive-owned impl, so the descriptor carries the host's guidance verbatim.
    assert_eq!(descriptor.instructions, hello_mcp::session_instructions());
    assert_eq!(descriptor.instructions.as_deref(), Some("hello from the minimal agentgear host"));
}

#[test]
fn embedded_tree_is_baked_in() {
    // The default `embed` feature compresses `plugin/` into the binary; an empty blob
    // would mean `setup` from `Source::Embedded` errors at materialize.
    assert!(!HelloMcp::embedded_blob().is_empty(), "the plugin tree was not embedded");
}

#[test]
fn restart_pending_is_none_before_install() {
    // The restart flag lives under `<data-dir>/hello-mcp/`, written only by a real
    // `install`/`update`. Nothing installed it here, so the notice is absent.
    //
    // A hermetic run would redirect HOME/XDG at a temp dir, but that needs
    // `std::env::set_var` (unsafe), which the workspace `unsafe_code = "forbid"` lint
    // bans; the per-plugin-name data dir keeps this deterministic without redirection.
    assert!(HelloMcp::restart_pending().is_none());
}
