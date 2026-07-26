//! Proves `PluginComponents::with_client` is reachable and usable from an
//! out-of-crate `AgentBackend` with no crate-internal access: only `agentgear`'s
//! public API (`PluginHost::descriptor`, `Plugin::components`, `Source::Path`,
//! `PluginComponents::with_client`) is used here, exactly as a third-party backend
//! crate would use it.
//!
//! A scratch plugin tree (shape (a) from the task: a tempdir tree parsed via
//! `Source::Path`, not hello-mcp's own shipped `plugin/`) is the right fixture
//! because hello-mcp's own tree carries no `${AGENTGEAR_CLIENT}` token and adding
//! one there would mean asserting on the shipped example's unrelated content.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use agentgear::{PluginHost, Source};
use hello_mcp::HelloMcp;

const PLUGIN_JSON: &str = r#"{
  "name": "client-token-fixture",
  "version": "0.0.0",
  "mcpServers": {
    "probe": {
      "command": "${AGENTGEAR_CLIENT}-mcp",
      "args": ["--client", "${AGENTGEAR_CLIENT}"]
    }
  }
}"#;

const HOOKS_JSON: &str = r#"{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "notify --client ${AGENTGEAR_CLIENT}" } ] }
    ]
  }
}"#;

#[test]
fn with_client_expands_the_token_for_an_out_of_crate_caller() {
    // `process::id()` disambiguates this run from any other test binary sharing
    // /tmp; nothing else in this file needs a second scratch tree.
    let dir = std::env::temp_dir().join(format!("ez-hello-mcp-client-token-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
    fs::create_dir_all(dir.join("hooks")).unwrap();
    fs::write(dir.join(".claude-plugin/plugin.json"), PLUGIN_JSON).unwrap();
    fs::write(dir.join("hooks/hooks.json"), HOOKS_JSON).unwrap();

    // The real entrypoint an out-of-crate AgentBackend has: the host's own
    // descriptor, `Plugin::components` for the IR, then `with_client` on it.
    let components = HelloMcp::descriptor().components(&Source::Path(dir.clone())).unwrap().with_client("some-client");

    let hook = components.hooks.iter().find(|h| h.event == "SessionStart").expect("hook missing");
    assert_eq!(hook.command, "notify --client some-client");
    assert!(!hook.command.contains("${AGENTGEAR_CLIENT}"), "literal token must not survive expansion");

    let mcp = components.mcp_servers.iter().find(|s| s.name == "probe").expect("mcp server missing");
    assert_eq!(mcp.command, "some-client-mcp");
    assert_eq!(mcp.args, vec!["--client".to_string(), "some-client".to_string()]);
    assert!(!mcp.command.contains("${AGENTGEAR_CLIENT}"), "literal token must not survive expansion");
    assert!(!mcp.args.iter().any(|a| a.contains("${AGENTGEAR_CLIENT}")), "literal token must not survive expansion");

    let _ = fs::remove_dir_all(&dir);
}
