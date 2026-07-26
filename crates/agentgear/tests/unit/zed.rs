//! zed backend unit tests: the mcp write/probe/remove cycle against a temp
//! `settings.json`. Zed is mcp-only, so the surface under test is `context_servers`
//! reconcile/probe/remove — asserting the exact Plain body, NoOp idempotency,
//! ownership-safe removal, and that non-stdio / `${CLAUDE_PLUGIN_ROOT}` servers are
//! never written (and therefore never removal candidates).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{check_mcp_registered, probe_mcp, reconcile_mcp, remove_mcp, writable_names};
use crate::agents::BackendState;
use crate::components::{McpKind, McpServer};
use crate::host::Outcome;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = crate::scratch::path("ez-zed-unit");
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

/// Linux/FreeBSD only: an absolute XDG value wins, a relative/absent one falls
/// back to `~/.config`. macOS deliberately has no counterpart here — zed hardcodes
/// `~/.config/zed` there and consults no env, so its arm takes no XDG input at all
/// (enforced by the type: `user_config_dir`'s macOS branch never reads the var).
#[cfg(not(any(windows, target_os = "macos")))]
#[test]
fn xdg_wins_only_when_absolute_else_home_config() {
    use std::path::{Path, PathBuf};

    use super::xdg_or_home_config;
    let home = || Some(PathBuf::from("/home/u"));
    assert_eq!(xdg_or_home_config(Some(PathBuf::from("/xdg")), home()), Some(PathBuf::from("/xdg/zed")));
    assert_eq!(xdg_or_home_config(Some(PathBuf::from("relative")), home()), Some(Path::new("/home/u/.config/zed").to_path_buf()));
    assert_eq!(xdg_or_home_config(None, home()), Some(Path::new("/home/u/.config/zed").to_path_buf()));
    assert_eq!(xdg_or_home_config(None, None), None);
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
fn writable_names_includes_http_and_excludes_non_portable_and_sse() {
    let http = McpServer {
        name: "remote-http".into(),
        kind: McpKind::Http { url: "https://example.test/mcp".into() },
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    let sse = McpServer {
        name: "remote-sse".into(),
        kind: McpKind::Sse { url: "https://example.test/sse".into() },
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    let servers = [stdio("ez-fixture", "host_fixture", &["mcp"]), stdio("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[]), http, sse];
    assert_eq!(writable_names(&servers), vec!["ez-fixture", "remote-http"]);
}

#[test]
fn reconcile_skips_non_portable_and_sse_servers_but_writes_http() {
    let path = scratch("settings.json");
    let http = McpServer {
        name: "remote-http".into(),
        kind: McpKind::Http { url: "https://example.test/mcp".into() },
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    let sse = McpServer {
        name: "remote-sse".into(),
        kind: McpKind::Sse { url: "https://example.test/sse".into() },
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    let servers = [stdio("ez-fixture", "host_fixture", &["mcp"]), stdio("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[]), http, sse];

    reconcile_mcp(&path, &servers).unwrap();
    let root = read(&path);
    assert!(root["context_servers"]["ez-fixture"].is_object(), "the portable stdio server must be written");
    assert_eq!(
        root["context_servers"]["remote-http"],
        serde_json::json!({"url": "https://example.test/mcp", "headers": {}}),
        "http must land in zed's {{url,headers}} form"
    );
    assert!(root["context_servers"].get("rooted").is_none(), "a CLAUDE_PLUGIN_ROOT-bearing server must be skipped");
    assert!(root["context_servers"].get("remote-sse").is_none(), "an sse server must be skipped (no zed landing)");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn a_dropped_server_warns_in_the_report() {
    // Zed's own mcp check, not the shared one: pins that this site folds the
    // skipped entries in rather than passing a bare `Ok` through.
    let rooted = stdio("ez", "${CLAUDE_PLUGIN_ROOT}/bin/ez", &[]);
    let sse = McpServer { name: "rm".into(), kind: McpKind::Sse { url: "https://x/sse".into() }, ..stdio("rm", "", &[]) };
    let check = check_mcp_registered(&[rooted, sse], None);
    let crate::doctor::CheckStatus::Warn(detail) = &check.status else {
        panic!("a dropped server must warn, got {:?}", check.status);
    };
    assert!(detail.contains("skipped ez: ${CLAUDE_PLUGIN_ROOT}"), "{detail}");
    assert!(detail.contains("skipped rm: this harness cannot host that transport"), "{detail}");
}
