//! mcpjson tests: render for Plain + Typed, and reconcile idempotency (Installed
//! on first write, NoOp on the second). Explicit temp paths, no env mutation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{ServerShape, reconcile, render_server};
use crate::components::{McpKind, McpServer};
use crate::host::Outcome;

fn server() -> McpServer {
    let mut env = BTreeMap::new();
    env.insert("K".to_string(), "V".to_string());
    McpServer { name: "ez".into(), kind: McpKind::Stdio, command: "host".into(), args: vec!["mcp".into()], env }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-mcpjson-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn renders_plain_and_typed() {
    let s = server();
    assert_eq!(render_server(&s, ServerShape::Plain), json!({"command":"host","args":["mcp"],"env":{"K":"V"}}));
    assert_eq!(render_server(&s, ServerShape::Typed), json!({"type":"stdio","command":"host","args":["mcp"],"env":{"K":"V"}}));
}

#[test]
fn reconcile_is_noop_on_second_call() {
    let path = scratch("settings.json");
    let servers = [server()];

    assert_eq!(reconcile(&path, &["mcpServers"], &servers, ServerShape::Plain).unwrap(), Outcome::Installed);
    assert_eq!(reconcile(&path, &["mcpServers"], &servers, ServerShape::Plain).unwrap(), Outcome::NoOp);

    let back: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(back["mcpServers"]["ez"], json!({"command":"host","args":["mcp"],"env":{"K":"V"}}));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
