//! jetbrains-copilot backend unit tests. The backend forwards mcp to the shared
//! json renderer with its own key (`servers`, not `mcpServers`) and the Typed
//! (`{type:"stdio",...}`) shape, so the tests drive that seam directly: exact
//! written shape, idempotent NoOp, user-entry-preserving removal, probe
//! classification, and the `${CLAUDE_PLUGIN_ROOT}` portability skip.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::portable_names;
use crate::agents::{BackendState, mcpjson};
use crate::components::{McpKind, McpServer};
use crate::host::Outcome;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-jbcopilot-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: vec!["mcp".into()], env: BTreeMap::new() }
}

fn read(path: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn reconcile_writes_servers_key_with_typed_stdio_shape() {
    let path = scratch("mcp.json");
    let servers = [server("ez-fixture", "host_fixture")];

    let outcome = mcpjson::reconcile(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap();
    assert_eq!(outcome, Outcome::Installed);

    let root = read(&path);
    // Root key is `servers`, NOT VS Code's / CC's `mcpServers`.
    assert!(root.get("mcpServers").is_none(), "must not write the `mcpServers` key:\n{root}");
    let entry = &root["servers"]["ez-fixture"];
    assert_eq!(entry["type"], "stdio", "stdio entries carry an explicit type:\n{entry}");
    assert_eq!(entry["command"], "host_fixture");
    assert_eq!(entry["args"][0], "mcp");
    assert!(entry.get("env").is_some(), "env object present:\n{entry}");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn second_reconcile_is_noop() {
    let path = scratch("mcp.json");
    let servers = [server("ez-fixture", "host_fixture")];

    assert_eq!(mcpjson::reconcile(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap(), Outcome::Installed);
    assert_eq!(
        mcpjson::reconcile(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap(),
        Outcome::NoOp,
        "a converged reconcile must not rewrite the file"
    );

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn remove_deletes_only_ours_and_keeps_a_pre_seeded_user_entry() {
    let path = scratch("mcp.json");
    // A foreign server + an unrelated top-level key that must survive our lifecycle.
    std::fs::write(&path, r#"{"inputs":[],"servers":{"theirs":{"type":"stdio","command":"their-server","args":[],"env":{}}}}"#).unwrap();

    let servers = [server("ez-fixture", "host_fixture")];
    mcpjson::reconcile(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap();

    let names = portable_names(&servers);
    let removed = mcpjson::remove(&path, super::MCP_KEY, &names).unwrap();
    assert_eq!(removed, Outcome::Removed);

    let root = read(&path);
    assert!(root["servers"].get("ez-fixture").is_none(), "our server survived removal:\n{root}");
    assert_eq!(root["servers"]["theirs"]["command"], "their-server", "seeded user server was clobbered:\n{root}");
    assert!(root.get("inputs").is_some(), "unrelated top-level key was dropped:\n{root}");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn probe_classifies_absent_healthy_and_needs_repair() {
    let path = scratch("mcp.json");
    let servers = [server("ez-fixture", "host_fixture")];

    // No file yet -> Absent.
    assert!(matches!(mcpjson::probe(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap(), BackendState::Absent));

    // Written and matching -> Healthy.
    mcpjson::reconcile(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap();
    assert!(matches!(mcpjson::probe(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap(), BackendState::Healthy));

    // Same key present but the desired body drifted -> NeedsRepair.
    let drifted = [server("ez-fixture", "different-binary")];
    assert!(matches!(mcpjson::probe(&path, super::MCP_KEY, &drifted, super::SHAPE).unwrap(), BackendState::NeedsRepair));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn non_portable_claude_plugin_root_server_is_skipped() {
    let path = scratch("mcp.json");
    let leaky = McpServer {
        name: "rooted".into(),
        kind: McpKind::Stdio,
        command: "${CLAUDE_PLUGIN_ROOT}/bin/leaky".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    // Mixed with a portable server so the write is deterministic: only the portable one
    // may land, the `${CLAUDE_PLUGIN_ROOT}` one is skipped (it can't run outside cc).
    let servers = [server("ez-fixture", "host_fixture"), leaky];

    mcpjson::reconcile(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap();
    let root = read(&path);
    assert!(root["servers"].get("ez-fixture").is_some(), "portable server should be written:\n{root}");
    assert!(root["servers"].get("rooted").is_none(), "non-portable server must be skipped:\n{root}");

    // ...and the skipped one is never a removal candidate either.
    assert_eq!(portable_names(&servers), vec!["ez-fixture"]);

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
