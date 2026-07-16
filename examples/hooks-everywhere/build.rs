// Every agentgear host ships this exact build.rs: it pins plugin.json `version` to
// CARGO_PKG_VERSION, tracks the plugin tree for rebuilds, and bakes it in as a blob.
fn main() {
    agentgear::build::assert_plugin_version();
}
