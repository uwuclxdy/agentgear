//! openclaw backend unit tests. The mcp glue is the shared json renderer keyed at
//! `mcp.servers` (two segments) with `ServerShape::plain()`, so these lock the
//! openclaw-specific pieces — the exact key path + body, portability filtering, an
//! idempotent second reconcile, exact removal that spares a user entry, and the
//! Absent/Healthy/NeedsRepair classification — against a throwaway config file.

use std::collections::BTreeMap;

use super::MCP_KEY;
use crate::agents::BackendState;
use crate::agents::mcpjson::{self, ServerShape};
use crate::components::{McpKind, McpServer};
use crate::host::Outcome;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-openclaw-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: vec!["mcp".into()], env: BTreeMap::new() }
}

/// A `${CLAUDE_PLUGIN_ROOT}`-bearing command only expands inside Claude Code, so
/// the backend must never write it (nor target it on removal).
fn rooted(name: &str) -> McpServer {
    McpServer {
        name: name.into(),
        kind: McpKind::Stdio,
        command: "${CLAUDE_PLUGIN_ROOT}/bin/x".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
    }
}

fn reconcile(path: &std::path::Path, servers: &[McpServer]) -> Outcome {
    mcpjson::reconcile(path, MCP_KEY, servers, ServerShape::plain()).unwrap()
}

fn probe(path: &std::path::Path, servers: &[McpServer]) -> BackendState {
    mcpjson::probe(path, MCP_KEY, servers, ServerShape::plain()).unwrap()
}

#[test]
fn reconcile_writes_plain_body_under_mcp_servers_and_is_idempotent() {
    let path = scratch("openclaw.json");
    let servers = [server("ez-fixture", "host_fixture")];

    assert_eq!(reconcile(&path, &servers), Outcome::Installed);

    let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let entry = root.get("mcp").and_then(|m| m.get("servers")).and_then(|s| s.get("ez-fixture")).expect("server under mcp.servers");
    assert_eq!(entry.get("command").and_then(|v| v.as_str()), Some("host_fixture"), "Plain body command: {entry}");
    assert_eq!(entry.get("args").and_then(|v| v.as_array()).map(Vec::len), Some(1), "Plain body args: {entry}");
    assert!(entry.get("env").is_some(), "Plain body must carry env: {entry}");

    // second reconcile with no drift is a true NoOp (no write).
    assert_eq!(reconcile(&path, &servers), Outcome::NoOp, "an already-converged config must not be rewritten");
    // present + byte-matching -> Healthy.
    assert!(matches!(probe(&path, &servers), BackendState::Healthy));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn probe_classifies_absent_healthy_and_needs_repair() {
    let path = scratch("openclaw.json");
    let servers = [server("ez-fixture", "host_fixture")];

    // no file -> Absent.
    assert!(matches!(probe(&path, &servers), BackendState::Absent), "a missing config is Absent");

    // only a foreign server present -> still Absent (ours is not there).
    std::fs::write(&path, r#"{"mcp":{"servers":{"theirs":{"command":"x","args":[],"env":{}}}}}"#).unwrap();
    assert!(matches!(probe(&path, &servers), BackendState::Absent), "a config without our key is Absent");

    // ours present but drifted (stale command) -> NeedsRepair.
    std::fs::write(&path, r#"{"mcp":{"servers":{"ez-fixture":{"command":"stale","args":["mcp"],"env":{}}}}}"#).unwrap();
    assert!(matches!(probe(&path, &servers), BackendState::NeedsRepair), "a drifted entry is NeedsRepair");

    // a converged reconcile flips it to Healthy.
    reconcile(&path, &servers);
    assert!(matches!(probe(&path, &servers), BackendState::Healthy));

    // an mcp-less plugin never drops a present marker: Healthy, not Absent.
    assert!(matches!(probe(&path, &[]), BackendState::Healthy), "no portable servers -> Healthy, never Absent");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn remove_deletes_only_ours_and_preserves_a_seeded_user_entry() {
    let path = scratch("openclaw.json");
    std::fs::write(&path, r#"{"theme":"dark","mcp":{"servers":{"theirs":{"command":"their-server","args":[],"env":{}}}}}"#).unwrap();
    let servers = [server("ez-fixture", "host_fixture")];

    reconcile(&path, &servers);
    let after_install = std::fs::read_to_string(&path).unwrap();
    assert!(after_install.contains("ez-fixture") && after_install.contains("theirs"), "install must merge, not clobber:\n{after_install}");

    let out = mcpjson::remove(&path, MCP_KEY, &servers, ServerShape::plain()).unwrap();
    assert_eq!(out, Outcome::Removed);
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(!after.contains("ez-fixture"), "our server survived remove:\n{after}");
    assert!(after.contains("theirs") && after.contains("their-server"), "user server clobbered by remove:\n{after}");
    assert!(after.contains("\"theme\"") && after.contains("dark"), "unrelated top-level key clobbered by remove:\n{after}");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn portable_filter_skips_claude_plugin_root_servers() {
    let servers = [server("ez-fixture", "host_fixture"), rooted("rooted")];

    let path = scratch("openclaw.json");
    reconcile(&path, &servers);
    let c = std::fs::read_to_string(&path).unwrap();
    assert!(c.contains("ez-fixture"), "portable server missing:\n{c}");
    assert!(!c.contains("rooted"), "a ${{CLAUDE_PLUGIN_ROOT}} server must never be written:\n{c}");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
