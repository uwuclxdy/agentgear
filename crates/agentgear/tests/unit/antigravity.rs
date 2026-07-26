//! antigravity backend unit tests: pins the desktop backend's MCP contract — the
//! `mcpServers` key + Plain `{command,args,env}` shape it shares byte-for-byte with
//! the `antigravity-cli` backend (so installing both is idempotent and their
//! removals are symmetric), plus the `${CLAUDE_PLUGIN_ROOT}` portability filter that
//! keeps a non-portable server out of both the write and the removal set. The
//! reconcile/probe/remove round-trip is driven straight through the shared renderer
//! at antigravity's chosen key + shape, so a future accidental shape change fails here.

use std::collections::BTreeMap;

use crate::agents::BackendState;
use crate::agents::mcpjson::{self, ServerShape};
use crate::components::{McpKind, McpServer};
use crate::host::Outcome;

const KEY: &[&str] = &["mcpServers"];

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = crate::scratch::path("ez-antigravity-unit");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: Vec::new(), env: BTreeMap::new() }
}

#[test]
fn reconcile_writes_plain_shape_then_no_ops_and_probe_tracks_state() {
    let path = scratch("mcp_config.json");
    let servers = [server("ez-fixture", "host_fixture")];

    // Absent before any write.
    assert!(matches!(mcpjson::probe(&path, KEY, &servers, ServerShape::plain()).unwrap(), BackendState::Absent));

    // reconcile writes our server under `mcpServers` in the Plain {command,args,env}
    // shape (byte-identical to the antigravity-cli backend's write to the same file).
    assert_eq!(mcpjson::reconcile(&path, KEY, &servers, ServerShape::plain()).unwrap(), Outcome::Installed);
    let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let entry = &root["mcpServers"]["ez-fixture"];
    assert_eq!(entry["command"], "host_fixture");
    assert_eq!(entry["args"], serde_json::json!([]));
    assert_eq!(entry["env"], serde_json::json!({}));
    assert!(entry.get("type").is_none(), "Plain shape must not emit a `type` field");

    // idempotent: a second reconcile with no drift is a true NoOp (no write).
    assert_eq!(mcpjson::reconcile(&path, KEY, &servers, ServerShape::plain()).unwrap(), Outcome::NoOp);
    assert!(matches!(mcpjson::probe(&path, KEY, &servers, ServerShape::plain()).unwrap(), BackendState::Healthy));

    // a drifted command flips probe to NeedsRepair (present but not matching).
    let drifted = [server("ez-fixture", "other-binary")];
    assert!(matches!(mcpjson::probe(&path, KEY, &drifted, ServerShape::plain()).unwrap(), BackendState::NeedsRepair));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn remove_deletes_only_ours_and_keeps_a_seeded_user_server() {
    let path = scratch("mcp_config.json");
    // A user's own server that must outlive our install + uninstall untouched.
    std::fs::write(&path, r#"{"mcpServers":{"theirs":{"command":"their-bin","args":[],"env":{}}}}"#).unwrap();

    let servers = [server("ez-fixture", "host_fixture")];
    assert_eq!(mcpjson::reconcile(&path, KEY, &servers, ServerShape::plain()).unwrap(), Outcome::Installed);

    // remove keys off the same writable filter reconcile uses (ours only), leaving the user's entry.
    assert_eq!(mcpjson::remove(&path, KEY, &servers, ServerShape::plain()).unwrap(), Outcome::Removed);
    let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(root["mcpServers"].get("ez-fixture").is_none(), "our server survived remove");
    assert_eq!(root["mcpServers"]["theirs"]["command"], "their-bin", "seeded user server was clobbered");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn non_portable_server_key_is_never_written_or_removed() {
    let path = scratch("mcp_config.json");
    let rooted = server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky");

    // The shared renderer skips a non-portable server: our entry's key is never
    // written (the wrapper `mcpServers` object may be created, but never `rooted`).
    mcpjson::reconcile(&path, KEY, std::slice::from_ref(&rooted), ServerShape::plain()).unwrap();
    if path.exists() {
        let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(root["mcpServers"].get("rooted").is_none(), "a non-portable server key must never be written");
    }

    // It is never a removal candidate either: seed a user server whose key equals the
    // non-portable one and prove remove leaves it byte-for-byte.
    std::fs::write(&path, r#"{"mcpServers":{"rooted":{"command":"user-rooted","args":[],"env":{}}}}"#).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    assert_eq!(mcpjson::remove(&path, KEY, std::slice::from_ref(&rooted), ServerShape::plain()).unwrap(), Outcome::NoOp);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "seeded same-name server survived byte-for-byte");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
