//! Host-facing build helper. The host authors a one-line `build.rs`:
//!
//! ```ignore
//! fn main() { ez_agent_plugin::build::assert_plugin_version(); }
//! ```
//!
//! It enforces `plugin.json` `version` == `CARGO_PKG_VERSION` at build time (CC
//! caches on version, so a mismatch would ship a silent no-op), tracks the tree
//! for rebuilds (`include_dir!` has no rebuild tracking of its own), and emits the
//! `EZ_PLUGIN_GUARD` env the derive's const-panic checks for.

use std::path::Path;

/// Assert against the default tree, `$CARGO_MANIFEST_DIR/plugin`.
pub fn assert_plugin_version() {
    let manifest_dir = env_or_panic("CARGO_MANIFEST_DIR");
    assert_plugin_version_at(Path::new(&manifest_dir).join("plugin"));
}

/// Assert against a non-default embedded tree dir (matches a custom `tree` attr).
pub fn assert_plugin_version_at(tree_dir: impl AsRef<Path>) {
    let tree_dir = tree_dir.as_ref();
    let plugin_json = tree_dir.join(".claude-plugin").join("plugin.json");
    let pkg_version = env_or_panic("CARGO_PKG_VERSION");

    let bytes = std::fs::read(&plugin_json).unwrap_or_else(|e| {
        panic!(
            "ez-agent-plugin: cannot read {} ({e}); ship the plugin tree there or call assert_plugin_version_at with the right dir",
            plugin_json.display()
        )
    });
    let manifest: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("ez-agent-plugin: {} is not valid JSON: {e}", plugin_json.display()));
    let plugin_version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("ez-agent-plugin: {} has no string `version`", plugin_json.display()));

    assert!(
        plugin_version == pkg_version,
        "ez-agent-plugin: plugin.json version `{plugin_version}` != CARGO_PKG_VERSION `{pkg_version}`; \
         bump Cargo.toml and plugin.json together (CC caches on version, so a mismatch ships a silent no-op)"
    );

    println!("cargo:rerun-if-changed={}", tree_dir.display());
    println!("cargo:rustc-env=EZ_PLUGIN_GUARD=1");
}

fn env_or_panic(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("ez-agent-plugin: {key} is not set (call this from a build.rs)"))
}
