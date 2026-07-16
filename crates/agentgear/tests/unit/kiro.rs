//! kiro backend unit tests: mcp/hook portability filtering, the hooks-into-an-
//! existing-agent-file merge (never fabricating one), exact removal that spares a
//! user's own entries, and the mcp probe classification the backend keys on.

use serde_json::Value;

use super::{event_supports_matcher, hook_is_portable, reconcile_hooks, remove_hooks, render_hook_entry};
use crate::agents::BackendState;
use crate::agents::mcpjson::{ServerShape, probe as mcp_probe};
use crate::components::{HookBinding, McpKind, McpServer};

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-kiro-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: vec!["mcp".into()], env: Default::default() }
}

fn hook(event: &str, command: &str) -> HookBinding {
    HookBinding { event: event.into(), matcher: None, command: command.into() }
}

/// A user-owned default agent seed: an agent identity plus one hook under an event
/// our fixture never emits, so removal must leave the whole file (minus our entries)
/// byte-identical.
const SEED_AGENT: &str = r#"{
  "name": "default",
  "description": "the user's default agent",
  "prompt": "you are helpful",
  "hooks": {
    "stop": [ { "command": "user-own-stop-hook" } ]
  }
}
"#;

#[test]
fn hook_portability_matches_mcp_server_rule() {
    assert!(hook_is_portable(&hook("SessionStart", "host_fixture self-heal")));
    assert!(!hook_is_portable(&hook("SessionStart", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh")));
}

#[test]
fn matcher_is_kept_only_for_tool_events() {
    assert!(event_supports_matcher("preToolUse") && event_supports_matcher("postToolUse"));
    assert!(!event_supports_matcher("agentSpawn") && !event_supports_matcher("stop"));

    // A tool-event hook keeps its matcher; a lifecycle event drops one it was given.
    let tool = HookBinding { event: "PreToolUse".into(), matcher: Some("execute_bash".into()), command: "guard".into() };
    let entry = render_hook_entry("preToolUse", &tool);
    assert_eq!(entry.get("matcher").and_then(Value::as_str), Some("execute_bash"));

    let lifecycle = HookBinding { event: "SessionStart".into(), matcher: Some("ignored".into()), command: "boot".into() };
    let entry = render_hook_entry("agentSpawn", &lifecycle);
    assert!(entry.get("matcher").is_none(), "a matcher is meaningless on a lifecycle event");
}

#[test]
fn reconcile_hooks_skips_when_the_agent_file_is_absent() {
    let path = scratch("default.json");
    let hooks = [hook("SessionStart", "host_fixture self-heal")];

    let changed = reconcile_hooks(&path, &hooks).unwrap();
    assert!(!changed, "hooks must not be written when there is no agent file");
    assert!(!path.exists(), "reconcile_hooks must never fabricate an agent definition");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn reconcile_merges_into_an_existing_agent_and_remove_spares_the_user() {
    let path = scratch("default.json");
    std::fs::write(&path, SEED_AGENT).unwrap();
    let hooks = [hook("SessionStart", "host_fixture self-heal"), hook("UserPromptSubmit", "host_fixture check-restart")];

    // reconcile: our two events land under `hooks`, camelCased, without our own
    // events being nested in the CC `{hooks:[{type,command}]}` shape.
    assert!(reconcile_hooks(&path, &hooks).unwrap(), "first reconcile writes");
    let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let events = root.get("hooks").and_then(Value::as_object).unwrap();
    assert_eq!(events["agentSpawn"][0]["command"], "host_fixture self-heal");
    assert_eq!(events["userPromptSubmit"][0]["command"], "host_fixture check-restart");
    // the user's identity + their own hook survived the merge.
    assert_eq!(root["name"], "default");
    assert_eq!(root["prompt"], "you are helpful");
    assert_eq!(events["stop"][0]["command"], "user-own-stop-hook");

    // second reconcile with no drift is a true NoOp (no write).
    assert!(!reconcile_hooks(&path, &hooks).unwrap(), "second reconcile must be a NoOp");

    // remove: our two events go, the user's `stop` hook + agent identity stay.
    assert!(remove_hooks(&path, &hooks).unwrap(), "remove strips our entries");
    let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let events = root.get("hooks").and_then(Value::as_object).unwrap();
    assert!(!events.contains_key("agentSpawn") && !events.contains_key("userPromptSubmit"), "our events must be gone");
    assert_eq!(events["stop"][0]["command"], "user-own-stop-hook", "the user's hook must survive");
    assert_eq!(root["name"], "default", "the user's agent identity must survive");

    // a second remove is a NoOp (nothing of ours left).
    assert!(!remove_hooks(&path, &hooks).unwrap(), "second remove must be a NoOp");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn remove_drops_a_hooks_object_it_empties() {
    let path = scratch("default.json");
    // An agent whose only hooks are ours: removal leaves no residual `hooks` key.
    std::fs::write(&path, r#"{"name":"default","hooks":{"agentSpawn":[{"command":"host_fixture self-heal"}]}}"#).unwrap();
    let hooks = [hook("SessionStart", "host_fixture self-heal")];

    assert!(remove_hooks(&path, &hooks).unwrap());
    let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(root.get("hooks").is_none(), "an emptied hooks object must be dropped, not left as `{{}}`");
    assert_eq!(root["name"], "default");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
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
