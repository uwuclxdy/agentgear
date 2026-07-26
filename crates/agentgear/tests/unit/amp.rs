//! amp backend unit tests: the load-bearing decision is amp's flat, dotted mcp
//! key (`"amp.mcpServers"` as one literal segment, never a nested `amp` object)
//! rendered through the shared `Plain` body. Exercised via the backend's own
//! `reconcile_mcp`/`probe_mcp`/`remove_mcp` wrappers so a key/shape change is
//! caught here, plus the portability filter shared with every non-CC backend.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{probe_mcp, reconcile_mcp, remove_mcp};
use crate::agents::BackendState;
use crate::components::{McpKind, McpServer};
use crate::host::Outcome;

fn srv(name: &str, command: &str, args: &[&str]) -> McpServer {
    McpServer {
        name: name.into(),
        kind: McpKind::Stdio,
        command: command.into(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
        env: BTreeMap::new(),
    }
}

fn scratch() -> std::path::PathBuf {
    let dir = crate::scratch::path("ez-amp-unit");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("settings.json")
}

fn read(path: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn cleanup(path: &std::path::Path) {
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn reconcile_writes_the_flat_amp_key_and_second_run_is_noop() {
    let path = scratch();
    let servers = [srv("ez-fixture", "host_fixture", &["mcp"])];

    assert_eq!(reconcile_mcp(&path, &servers).unwrap(), Outcome::Installed);

    let v = read(&path);
    // The literal VS-Code-style flat key — never a nested `amp` object.
    assert!(v.get("amp").is_none(), "must not nest servers under an `amp` object:\n{v:#}");
    assert_eq!(v["amp.mcpServers"]["ez-fixture"], json!({ "command": "host_fixture", "args": ["mcp"], "env": {} }));

    // idempotent: no drift -> NoOp, and the file's bytes are untouched.
    let before = std::fs::read(&path).unwrap();
    assert_eq!(reconcile_mcp(&path, &servers).unwrap(), Outcome::NoOp);
    assert_eq!(std::fs::read(&path).unwrap(), before, "a no-drift reconcile rewrote the file");

    cleanup(&path);
}

#[test]
fn remove_strips_only_our_key_and_keeps_the_user_config() {
    let path = scratch();
    // A foreign server under the same flat key + an unrelated top-level key.
    std::fs::write(&path, r#"{"theme":"dark","amp.mcpServers":{"theirs":{"command":"their-server"}}}"#).unwrap();
    let servers = [srv("ez-fixture", "host_fixture", &["mcp"])];

    reconcile_mcp(&path, &servers).unwrap();
    let v = read(&path);
    assert!(v["amp.mcpServers"].get("ez-fixture").is_some(), "our server was not merged in");
    assert!(v["amp.mcpServers"].get("theirs").is_some(), "user server was clobbered on install");

    assert_eq!(remove_mcp(&path, &servers).unwrap(), Outcome::Removed);
    let v = read(&path);
    assert!(v["amp.mcpServers"].get("ez-fixture").is_none(), "our server survived remove");
    assert!(v["amp.mcpServers"].get("theirs").is_some(), "user server was clobbered on remove");
    assert_eq!(v["theme"], json!("dark"), "unrelated top-level key was clobbered");

    cleanup(&path);
}

#[test]
fn probe_classifies_absent_healthy_and_needs_repair() {
    let path = scratch();
    let servers = [srv("ez-fixture", "host_fixture", &["mcp"])];

    // Absent: no config file at all.
    assert!(matches!(probe_mcp(&path, &servers).unwrap(), BackendState::Absent));

    // Healthy: exactly our render present.
    reconcile_mcp(&path, &servers).unwrap();
    assert!(matches!(probe_mcp(&path, &servers).unwrap(), BackendState::Healthy));

    // NeedsRepair: our key present but drifted (a hand-edit of the args).
    let mut v = read(&path);
    v["amp.mcpServers"]["ez-fixture"]["args"] = json!(["tampered"]);
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(matches!(probe_mcp(&path, &servers).unwrap(), BackendState::NeedsRepair));

    cleanup(&path);
}

#[test]
fn reconcile_refuses_a_jsonc_shadow_instead_of_writing_settings_json() {
    let path = scratch();
    let jsonc = path.with_extension("jsonc");
    // A user's comment-bearing config amp reads but json_edit can't parse/merge.
    std::fs::write(&jsonc, "{\n  // my servers\n  \"amp.mcpServers\": {}\n}\n").unwrap();
    let servers = [srv("ez-fixture", "host_fixture", &["mcp"])];

    let err = reconcile_mcp(&path, &servers).unwrap_err();
    assert!(matches!(err, crate::error::Error::Config { .. }), "expected a Config refusal, got: {err:?}");
    // Must NOT create a shadowing settings.json beside the user's .jsonc.
    assert!(!path.exists(), "reconcile wrote a shadowing settings.json instead of refusing");

    cleanup(&path);
}

#[test]
fn non_portable_servers_are_never_written_or_removed() {
    let path = scratch();
    let rooted = srv("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[]);

    // A ${CLAUDE_PLUGIN_ROOT} server is skipped — never written under our key.
    reconcile_mcp(&path, std::slice::from_ref(&rooted)).unwrap();
    let v = read(&path);
    assert!(v["amp.mcpServers"].get("rooted").is_none(), "a ${{CLAUDE_PLUGIN_ROOT}} server must never be written");

    // A user's own server that happens to share the non-portable name survives
    // remove untouched: it is not in `portable_names`, so we never target it.
    std::fs::write(&path, r#"{"amp.mcpServers":{"rooted":{"command":"users-own"}}}"#).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    assert_eq!(remove_mcp(&path, std::slice::from_ref(&rooted)).unwrap(), Outcome::NoOp);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "remove touched a server it never wrote");

    cleanup(&path);
}
