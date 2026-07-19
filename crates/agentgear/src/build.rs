//! Host-facing build helper. The host authors a one-line `build.rs`:
//!
//! ```ignore
//! fn main() { agentgear::build::assert_plugin_version(); }
//! ```
//!
//! It enforces `plugin.json` `version` == `CARGO_PKG_VERSION` at build time (CC
//! caches on version, so a mismatch would ship a silent no-op), tracks the tree
//! for rebuilds, and emits the `AGENTGEAR_GUARD` env the derive's const-panic
//! checks for. It also emits a `cargo:warning` naming any plugin entry that carries
//! `${CLAUDE_PLUGIN_ROOT}` (see [`warn_non_portable`]). With the `embed` feature
//! (default) it also tars + brotli-compresses the tree to `$OUT_DIR/agentgear.tar.br`
//! and emits `AGENTGEAR_BLOB` (the path the derive's `include_bytes!` bakes in).

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
            "agentgear: cannot read {} ({e}); ship the plugin tree there or call assert_plugin_version_at with the right dir",
            plugin_json.display()
        )
    });
    let manifest: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("agentgear: {} is not valid JSON: {e}", plugin_json.display()));
    let plugin_version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("agentgear: {} has no string `version`", plugin_json.display()));

    assert!(
        plugin_version == pkg_version,
        "agentgear: plugin.json version `{plugin_version}` != CARGO_PKG_VERSION `{pkg_version}`; \
         bump Cargo.toml and plugin.json together (CC caches on version, so a mismatch ships a silent no-op)"
    );

    println!("cargo:rerun-if-changed={}", tree_dir.display());
    println!("cargo:rustc-env=AGENTGEAR_GUARD=1");

    warn_non_portable(tree_dir);

    #[cfg(feature = "embed")]
    embed_blob(tree_dir);
}

/// Emit a `cargo:warning` naming every plugin entry (mcp server or hook) whose
/// command or args carry `${CLAUDE_PLUGIN_ROOT}`. That token only expands inside
/// Claude Code's own runtime, so every non-Claude backend skips such an entry at
/// install time (the shared renderers enforce it). Advisory and unconditional: it
/// runs even without the `embed` feature, and a Claude-Code-only host still sees it
/// (the canonical plugin shape puts the token in an mcp server's args), which is why
/// the wording frames it as informational. A tree that fails to read or parse is
/// left to surface at install time rather than failing the build here.
fn warn_non_portable(tree_dir: &Path) {
    let Ok(entries) = crate::materialize::dir_entries(tree_dir) else {
        return;
    };
    let Ok(components) = crate::components::PluginComponents::parse(&entries) else {
        return;
    };
    if let Some(warning) = non_portable_warning(&components) {
        println!("cargo:warning={warning}");
    }
}

/// The single-line warning for the non-portable entries in `components`, or `None`
/// when every entry is portable. Split from [`warn_non_portable`] so a unit test can
/// assert it names the non-portable entries and omits the portable ones without a
/// build-script run.
fn non_portable_warning(components: &crate::components::PluginComponents) -> Option<String> {
    let mut entries = Vec::new();
    for server in &components.mcp_servers {
        if !server.is_portable() {
            entries.push(format!("mcp server `{}`", server.name));
        }
    }
    for hook in &components.hooks {
        if !hook.is_portable() {
            entries.push(format!("`{}` hook", hook.event));
        }
    }
    if entries.is_empty() {
        return None;
    }
    Some(format!(
        "agentgear: {} use ${{CLAUDE_PLUGIN_ROOT}}, which only expands inside Claude Code, so every non-Claude backend skips them. \
         This is the canonical shape for a Claude-Code-only plugin; it only matters if you also target another agent.",
        entries.join(", ")
    ))
}

/// Compress the tree to `$OUT_DIR/agentgear.tar.br` and point the derive's
/// `include_bytes!` at it via `AGENTGEAR_BLOB`.
#[cfg(feature = "embed")]
fn embed_blob(tree_dir: &Path) {
    let out_dir = env_or_panic("OUT_DIR");
    let blob = crate::materialize::compress_dir(tree_dir)
        .unwrap_or_else(|e| panic!("agentgear: failed to compress the plugin tree at {} ({e})", tree_dir.display()));
    let blob_path = Path::new(&out_dir).join("agentgear.tar.br");
    std::fs::write(&blob_path, &blob).unwrap_or_else(|e| panic!("agentgear: cannot write {} ({e})", blob_path.display()));
    println!("cargo:rustc-env=AGENTGEAR_BLOB={}", blob_path.display());
}

fn env_or_panic(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("agentgear: {key} is not set (call this from a build.rs)"))
}

#[cfg(test)]
#[path = "../tests/unit/build.rs"]
mod build_tests;
