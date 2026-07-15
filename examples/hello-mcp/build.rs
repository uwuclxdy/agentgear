// The only build.rs a host needs: it pins plugin.json `version` to CARGO_PKG_VERSION,
// tracks the plugin tree for rebuilds, and bakes it in as a compressed blob.
fn main() {
    agentgear::build::assert_plugin_version();
}
