//! openclaw backend unit tests. The mcp glue is the shared json renderer keyed at
//! `mcp.servers` (two segments) with `SHAPE` (stdio `ServerShape::plain()`, remote
//! `RemoteShape::UrlHeadersTransport`), so these lock the openclaw-specific pieces —
//! the exact key path + body, portability filtering, an idempotent second reconcile,
//! exact removal that spares a user entry, the Absent/Healthy/NeedsRepair
//! classification, and that our remote render matches openclaw's own canonical
//! post-`doctor --fix` shape byte-for-byte — against a throwaway config file.

use std::collections::BTreeMap;

use super::{MCP_KEY, SHAPE};
use crate::agents::BackendState;
use crate::agents::mcpjson::{self, render_server};
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
    mcpjson::reconcile(path, MCP_KEY, servers, SHAPE).unwrap()
}

fn probe(path: &std::path::Path, servers: &[McpServer]) -> BackendState {
    mcpjson::probe(path, MCP_KEY, servers, SHAPE).unwrap()
}

fn remote_server(name: &str, kind: McpKind) -> McpServer {
    McpServer { name: name.into(), kind, command: String::new(), args: Vec::new(), env: BTreeMap::new() }
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

/// Pins the exact canonical shape from `docs/research/verify-openclaw.md` #2/#4:
/// `openclaw doctor --fix` rewrote agentgear's `{type,url,headers}` render into
/// `{url,headers,transport}` on disk. Our render must equal that body directly
/// (never the `type`-keyed one), and a probe against a config already in that
/// post-canonicalization shape must read `Healthy`, not churn into `NeedsRepair`.
#[test]
fn remote_render_matches_openclaws_canonical_post_doctor_fix_shape() {
    let http = remote_server("ez-http", McpKind::Http { url: "http://127.0.0.1:9/mcp".into() });
    let sse = remote_server("ez-sse", McpKind::Sse { url: "http://127.0.0.1:9/sse".into() });

    assert_eq!(
        render_server(&http, SHAPE).unwrap(),
        serde_json::json!({"url": "http://127.0.0.1:9/mcp", "headers": {}, "transport": "streamable-http"})
    );
    assert_eq!(
        render_server(&sse, SHAPE).unwrap(),
        serde_json::json!({"url": "http://127.0.0.1:9/sse", "headers": {}, "transport": "sse"})
    );

    let path = scratch("openclaw.json");
    // Seed the file exactly as `doctor --fix` would leave it after canonicalizing
    // agentgear's own (now-stale) `{type,url,headers}` render — no `type` key.
    std::fs::write(
        &path,
        r#"{"mcp":{"servers":{
            "ez-http": {"url":"http://127.0.0.1:9/mcp","headers":{},"transport":"streamable-http"},
            "ez-sse": {"url":"http://127.0.0.1:9/sse","headers":{},"transport":"sse"}
        }}}"#,
    )
    .unwrap();
    assert!(matches!(probe(&path, &[http, sse]), BackendState::Healthy), "canonical on-disk shape must read Healthy, not churn");

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

    let out = mcpjson::remove(&path, MCP_KEY, &servers, SHAPE).unwrap();
    assert_eq!(out, Outcome::Removed);
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(!after.contains("ez-fixture"), "our server survived remove:\n{after}");
    assert!(after.contains("theirs") && after.contains("their-server"), "user server clobbered by remove:\n{after}");
    assert!(after.contains("\"theme\"") && after.contains("dark"), "unrelated top-level key clobbered by remove:\n{after}");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn openclaw_home_reads_one_level_deeper() {
    // OPENCLAW_HOME is a HOME-equivalent: openclaw itself reads `<dir>/.openclaw/openclaw.json`.
    let dir = std::path::PathBuf::from("/scratch/oc-home");
    let got = super::config_path_from(None, None, Some(dir.clone()), None);
    assert_eq!(got, Some(dir.join(".openclaw").join("openclaw.json")), "OPENCLAW_HOME must resolve one level deeper");
}

#[test]
fn openclaw_state_dir_is_flat() {
    // OPENCLAW_STATE_DIR already points at the state dir itself: no extra `.openclaw` level.
    let dir = std::path::PathBuf::from("/scratch/oc-state");
    let got = super::config_path_from(None, Some(dir.clone()), None, None);
    assert_eq!(got, Some(dir.join("openclaw.json")), "OPENCLAW_STATE_DIR must resolve flat");
}

#[test]
fn config_path_precedence_config_beats_state_beats_home_beats_default() {
    let config = std::path::PathBuf::from("/scratch/explicit.json");
    let state = std::path::PathBuf::from("/scratch/state");
    let home_override = std::path::PathBuf::from("/scratch/home-override");
    let default_home = std::path::PathBuf::from("/scratch/default-home");

    // OPENCLAW_CONFIG_PATH wins over everything else.
    assert_eq!(
        super::config_path_from(Some(config.clone()), Some(state.clone()), Some(home_override.clone()), Some(default_home.clone())),
        Some(config)
    );
    // OPENCLAW_STATE_DIR beats OPENCLAW_HOME and the default.
    assert_eq!(
        super::config_path_from(None, Some(state.clone()), Some(home_override.clone()), Some(default_home.clone())),
        Some(state.join("openclaw.json"))
    );
    // OPENCLAW_HOME beats the default.
    assert_eq!(
        super::config_path_from(None, None, Some(home_override.clone()), Some(default_home.clone())),
        Some(home_override.join(".openclaw").join("openclaw.json"))
    );
    // nothing overridden -> the default home, same HOME-equivalent join.
    assert_eq!(
        super::config_path_from(None, None, None, Some(default_home.clone())),
        Some(default_home.join(".openclaw").join("openclaw.json"))
    );
    // no home at all -> None (matches `config_path()`'s error path).
    assert_eq!(super::config_path_from(None, None, None, None), None);
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
