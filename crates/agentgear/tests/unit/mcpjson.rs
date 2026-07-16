//! mcpjson tests: render for the stdio + remote dialects, reconcile idempotency
//! (Installed on first write, NoOp on the second), and remove ownership (a server
//! we never wrote is never deleted). Explicit temp paths, no env mutation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{ServerShape, reconcile, remove, render_server};
use crate::components::{McpKind, McpServer};
use crate::host::Outcome;

fn server() -> McpServer {
    let mut env = BTreeMap::new();
    env.insert("K".to_string(), "V".to_string());
    McpServer { name: "ez".into(), kind: McpKind::Stdio, command: "host".into(), args: vec!["mcp".into()], env }
}

fn remote_server(name: &str, kind: McpKind) -> McpServer {
    McpServer { name: name.into(), kind, command: String::new(), args: vec![], env: BTreeMap::new() }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-mcpjson-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn renders_plain_and_typed() {
    let s = server();
    assert_eq!(render_server(&s, ServerShape::plain()), json!({"command":"host","args":["mcp"],"env":{"K":"V"}}));
    assert_eq!(render_server(&s, ServerShape::typed()), json!({"type":"stdio","command":"host","args":["mcp"],"env":{"K":"V"}}));
}

#[test]
fn renders_remote_type_url_headers() {
    let http = remote_server("h", McpKind::Http { url: "https://x/mcp".into() });
    let sse = remote_server("s", McpKind::Sse { url: "https://x/sse".into() });
    // The remote dialect is independent of the stdio one: plain and typed agree.
    for shape in [ServerShape::plain(), ServerShape::typed()] {
        assert_eq!(render_server(&http, shape), json!({"type":"http","url":"https://x/mcp","headers":{}}));
        assert_eq!(render_server(&sse, shape), json!({"type":"sse","url":"https://x/sse","headers":{}}));
    }
}

#[test]
fn remove_never_touches_a_server_we_could_not_have_written() {
    let path = scratch("settings.json");
    // A user's own entry named like our non-portable server, which reconcile skips.
    std::fs::write(&path, r#"{"mcpServers":{"ez":{"command":"users-own"},"other":{"command":"x"}}}"#).unwrap();
    let mut ours = server();
    ours.command = "${CLAUDE_PLUGIN_ROOT}/bin".into();

    assert_eq!(remove(&path, &["mcpServers"], &[ours], ServerShape::plain()).unwrap(), Outcome::NoOp);
    let back: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(back["mcpServers"]["ez"], json!({"command":"users-own"}), "user's same-named server was deleted");

    // The portable case still removes exactly our key.
    assert_eq!(remove(&path, &["mcpServers"], &[server()], ServerShape::plain()).unwrap(), Outcome::Removed);
    let back: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(back["mcpServers"].get("ez").is_none(), "our key survived remove");
    assert_eq!(back["mcpServers"]["other"], json!({"command":"x"}), "unrelated server was deleted");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn reconcile_is_noop_on_second_call() {
    let path = scratch("settings.json");
    let servers = [server()];

    assert_eq!(reconcile(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), Outcome::Installed);
    assert_eq!(reconcile(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), Outcome::NoOp);

    let back: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(back["mcpServers"]["ez"], json!({"command":"host","args":["mcp"],"env":{"K":"V"}}));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
