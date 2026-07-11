//! zed backend unit tests: the mcp write/probe/remove cycle against a temp
//! `settings.json`. Zed is mcp-only, so the surface under test is `context_servers`
//! reconcile/probe/remove — asserting the exact Plain body, NoOp idempotency,
//! ownership-safe removal, and that non-stdio / `${CLAUDE_PLUGIN_ROOT}` servers are
//! never written (and therefore never removal candidates).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{probe_mcp, reconcile_mcp, remove_mcp, writable_names};
use crate::agents::BackendState;
use crate::components::{McpKind, McpServer};
use crate::host::Outcome;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-zed-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn stdio(name: &str, command: &str, args: &[&str]) -> McpServer {
    McpServer {
        name: name.into(),
        kind: McpKind::Stdio,
        command: command.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        env: BTreeMap::new(),
    }
}

fn read(path: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn reconcile_writes_flat_context_servers_body_and_second_is_noop() {
    let path = scratch("settings.json");
    let server = stdio("ez-fixture", "host_fixture", &["mcp"]);

    let out = reconcile_mcp(&path, std::slice::from_ref(&server)).unwrap();
    assert_eq!(out, Outcome::Installed);

    // Exact shape: flat {command,args,env} under `context_servers.<name>`, no `type`.
    let root = read(&path);
    assert_eq!(root["context_servers"]["ez-fixture"], json!({ "command": "host_fixture", "args": ["mcp"], "env": {} }));

    // Idempotent: an unchanged reconcile does not rewrite the file.
    let out = reconcile_mcp(&path, std::slice::from_ref(&server)).unwrap();
    assert_eq!(out, Outcome::NoOp);

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn probe_classifies_absent_healthy_and_needs_repair() {
    let path = scratch("settings.json");
    let server = stdio("ez-fixture", "host_fixture", &["mcp"]);

    // No file yet -> Absent.
    assert!(matches!(probe_mcp(&path, std::slice::from_ref(&server)).unwrap(), BackendState::Absent));

    reconcile_mcp(&path, std::slice::from_ref(&server)).unwrap();
    // Present and matching -> Healthy.
    assert!(matches!(probe_mcp(&path, std::slice::from_ref(&server)).unwrap(), BackendState::Healthy));

    // Present but drifted (our key exists with a different body) -> NeedsRepair.
    let drifted = stdio("ez-fixture", "host_fixture", &["mcp", "--v2"]);
    assert!(matches!(probe_mcp(&path, std::slice::from_ref(&drifted)).unwrap(), BackendState::NeedsRepair));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn remove_deletes_only_ours_and_preserves_a_user_entry() {
    let path = scratch("settings.json");
    // A user's own context server + an unrelated top-level key that must both survive.
    std::fs::write(&path, r#"{"theme":"One Dark","context_servers":{"theirs":{"command":"their-server","args":[],"env":{}}}}"#).unwrap();
    let server = stdio("ez-fixture", "host_fixture", &["mcp"]);

    reconcile_mcp(&path, std::slice::from_ref(&server)).unwrap();
    let root = read(&path);
    assert!(root["context_servers"]["ez-fixture"].is_object(), "our server was not written");
    assert!(root["context_servers"]["theirs"].is_object(), "the user's server was clobbered on reconcile");

    let out = remove_mcp(&path, std::slice::from_ref(&server)).unwrap();
    assert_eq!(out, Outcome::Removed);

    let root = read(&path);
    assert!(root["context_servers"].get("ez-fixture").is_none(), "our server survived remove");
    assert_eq!(root["context_servers"]["theirs"], json!({ "command": "their-server", "args": [], "env": {} }), "user's server was touched");
    assert_eq!(root["theme"], json!("One Dark"), "unrelated top-level key was touched");

    // A second remove finds nothing of ours left -> NoOp.
    assert_eq!(remove_mcp(&path, std::slice::from_ref(&server)).unwrap(), Outcome::NoOp);

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn writable_names_excludes_non_portable_and_non_stdio() {
    let http = McpServer {
        name: "remote".into(),
        kind: McpKind::Http { url: "https://example.test".into() },
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    let servers = [stdio("ez-fixture", "host_fixture", &["mcp"]), stdio("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[]), http];
    assert_eq!(writable_names(&servers), vec!["ez-fixture"]);
}

#[test]
fn reconcile_skips_non_portable_and_non_stdio_servers() {
    let path = scratch("settings.json");
    let http = McpServer {
        name: "remote".into(),
        kind: McpKind::Sse { url: "https://example.test".into() },
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    let servers = [stdio("ez-fixture", "host_fixture", &["mcp"]), stdio("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[]), http];

    reconcile_mcp(&path, &servers).unwrap();
    let root = read(&path);
    assert!(root["context_servers"]["ez-fixture"].is_object(), "the portable stdio server must be written");
    assert!(root["context_servers"].get("rooted").is_none(), "a CLAUDE_PLUGIN_ROOT-bearing server must be skipped");
    assert!(root["context_servers"].get("remote").is_none(), "a non-stdio (remote) server must be skipped");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
