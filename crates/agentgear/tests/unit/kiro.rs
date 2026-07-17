//! kiro backend unit tests: the mcp probe classification the backend keys on.
//! kiro is mcp-only (hooks are declared unsupported — its only hook surface is a
//! user-owned per-agent config json; the hermetic test pins that the backend never
//! touches an agent file), so the mcp seam is the whole unit surface.

use crate::agents::BackendState;
use crate::agents::mcpjson::{ServerShape, probe as mcp_probe};
use crate::components::{McpKind, McpServer};

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-kiro-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: vec!["mcp".into()], env: Default::default() }
}

#[test]
fn probe_classifies_absent_healthy_and_needs_repair() {
    let path = scratch("mcp.json");
    let servers = [server("ez-fixture", "host_fixture")];
    let key = ["mcpServers"];

    // no mcp.json yet -> Absent (our portable server is genuinely not installed).
    assert!(matches!(mcp_probe(&path, &key, &servers, ServerShape::plain()).unwrap(), BackendState::Absent));

    // present + matching the Plain render -> Healthy.
    std::fs::write(&path, r#"{"mcpServers":{"ez-fixture":{"command":"host_fixture","args":["mcp"],"env":{}}}}"#).unwrap();
    assert!(matches!(mcp_probe(&path, &key, &servers, ServerShape::plain()).unwrap(), BackendState::Healthy));

    // present but drifted (different command) -> NeedsRepair.
    std::fs::write(&path, r#"{"mcpServers":{"ez-fixture":{"command":"other","args":["mcp"],"env":{}}}}"#).unwrap();
    assert!(matches!(mcp_probe(&path, &key, &servers, ServerShape::plain()).unwrap(), BackendState::NeedsRepair));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
