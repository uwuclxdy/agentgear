// Every agentgear host ships this exact build.rs: it pins plugin.json `version` to
// CARGO_PKG_VERSION and tracks the plugin tree for rebuilds. With the `embed` feature
// off (this host is zero-embed) it skips baking the tree into the binary.
fn main() {
    agentgear::build::assert_plugin_version();
}
