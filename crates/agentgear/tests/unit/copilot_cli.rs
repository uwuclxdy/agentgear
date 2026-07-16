//! copilot-cli backend unit tests: the bespoke `mcp-config.json` render (copilot's
//! `type:"local"` + `tools:["*"]` shape), the whole-file-owned `hooks/<plugin>.json`
//! (event-name mapping, idempotent write, delete-only-what-we-wrote), agent
//! frontmatter, and the portability filter both mcp and hooks share (a
//! `${CLAUDE_PLUGIN_ROOT}`-bearing entry is never written, so never a removal target).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use super::BackendState;
use super::{
    agent_file, hook_is_portable, map_event, portable_names, probe_mcp, reconcile_hooks, reconcile_mcp, remove_hooks, remove_mcp,
    render_agent,
};
use crate::components::{HookBinding, MarkdownDoc, McpKind, McpServer};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-copilot-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: vec!["mcp".into()], env: BTreeMap::new() }
}

fn hook(event: &str, command: &str) -> HookBinding {
    HookBinding { event: event.into(), matcher: None, command: command.into() }
}

fn agent_doc() -> MarkdownDoc {
    let mut fm = BTreeMap::new();
    fm.insert("name".into(), Value::String("ez-helper".into()));
    fm.insert("description".into(), Value::String("a: fixture \"agent\"".into()));
    fm.insert("model".into(), Value::String("sonnet".into()));
    MarkdownDoc {
        name: "ez-helper".into(),
        rel: "agents/ez-helper.md".into(),
        frontmatter: fm,
        body: "Do the thing.\n".into(),
        raw: Vec::new(),
    }
}

// --- mcp ---------------------------------------------------------------------

#[test]
fn portable_names_excludes_claude_plugin_root_servers() {
    let servers = [server("ez-fixture", "host_fixture"), server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky")];
    assert_eq!(portable_names(&servers), vec!["ez-fixture"]);
}

#[test]
fn reconcile_mcp_writes_copilot_shape_and_keeps_a_seeded_user_server() {
    let path = scratch("mcp-config.json");
    // A user server we must never clobber.
    std::fs::write(&path, r#"{"mcpServers":{"theirs":{"type":"local","command":"their-server"}}}"#).unwrap();

    let servers = [server("ez-fixture", "host_fixture"), server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky")];
    assert!(reconcile_mcp(&path, &servers).unwrap(), "reconcile must report a change on first write");

    let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let ours = &root["mcpServers"]["ez-fixture"];
    assert_eq!(ours["type"], Value::from("local"), "stdio must render copilot's `local` type, not `stdio`");
    assert_eq!(ours["command"], Value::from("host_fixture"));
    assert_eq!(ours["args"], serde_json::json!(["mcp"]));
    assert_eq!(
        ours["tools"],
        serde_json::json!(["*"]),
        "tools must default to the `[\"*\"]` array (a bare string voids copilot's whole mcp file)"
    );
    assert!(ours["env"].is_object());
    assert!(root["mcpServers"].get("rooted").is_none(), "a ${{CLAUDE_PLUGIN_ROOT}} server must never be written");
    assert!(root["mcpServers"].get("theirs").is_some(), "seeded user server was clobbered");

    // Second reconcile with no drift is a true NoOp (no write).
    assert!(!reconcile_mcp(&path, &servers).unwrap(), "second reconcile must be a NoOp");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn probe_mcp_classifies_absent_healthy_and_needs_repair() {
    let path = scratch("mcp-config.json");
    let servers = [server("ez-fixture", "host_fixture")];

    // No file yet -> Absent.
    assert!(matches!(probe_mcp(&path, &servers).unwrap(), BackendState::Absent));

    // A plugin with no portable server is Healthy (never Absent), so self_heal keeps
    // a present marker — even before any reconcile writes the file (an mcp-less plugin
    // early-returns from reconcile_mcp, so the file never exists; a missing file must
    // not short-circuit to Absent ahead of the empty-portable check).
    let rooted = [server("rooted", "${CLAUDE_PLUGIN_ROOT}/x")];
    assert!(matches!(probe_mcp(&path, &rooted).unwrap(), BackendState::Healthy), "missing file + no portable server must be Healthy");
    std::fs::write(&path, "{}").unwrap();
    assert!(matches!(probe_mcp(&path, &rooted).unwrap(), BackendState::Healthy));

    // After a real reconcile the render byte-matches -> Healthy.
    reconcile_mcp(&path, &servers).unwrap();
    assert!(matches!(probe_mcp(&path, &servers).unwrap(), BackendState::Healthy));

    // A drifted entry under our key -> NeedsRepair.
    std::fs::write(&path, r#"{"mcpServers":{"ez-fixture":{"type":"local","command":"stale"}}}"#).unwrap();
    assert!(matches!(probe_mcp(&path, &servers).unwrap(), BackendState::NeedsRepair));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn remove_mcp_drops_only_our_key() {
    let path = scratch("mcp-config.json");
    let servers = [server("ez-fixture", "host_fixture")];
    reconcile_mcp(&path, &servers).unwrap();
    // Add a foreign server beside ours.
    let mut root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    root["mcpServers"]["theirs"] = serde_json::json!({ "type": "local", "command": "their-server" });
    std::fs::write(&path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();

    assert!(remove_mcp(&path, &portable_names(&servers)).unwrap());
    let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(root["mcpServers"].get("ez-fixture").is_none(), "our server survived remove");
    assert!(root["mcpServers"].get("theirs").is_some(), "remove deleted the user's server");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

// --- hooks -------------------------------------------------------------------

#[test]
fn map_event_uses_copilots_camelcase_names() {
    assert_eq!(map_event("SessionStart"), Some("sessionStart"));
    assert_eq!(map_event("UserPromptSubmit"), Some("userPromptSubmitted"));
    assert_eq!(map_event("Stop"), Some("agentStop"));
    assert_eq!(map_event("PreToolUse"), Some("preToolUse"));
    // A CC event copilot does not define is skipped, never guessed.
    assert_eq!(map_event("PreToolUseNope"), None);
}

#[test]
fn hook_portability_matches_mcp_server_rule() {
    assert!(hook_is_portable(&hook("SessionStart", "host_fixture self-heal")));
    assert!(!hook_is_portable(&hook("SessionStart", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh")));
}

#[test]
fn reconcile_hooks_writes_owned_file_and_is_idempotent() {
    let path = scratch("ez-fixture-plugin.json");
    let hooks = [hook("SessionStart", "host_fixture self-heal"), hook("UserPromptSubmit", "host_fixture check-restart")];

    assert!(reconcile_hooks(&path, &hooks).unwrap(), "first write must report a change");
    let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(root["version"], Value::from(1));
    assert_eq!(root["disableAllHooks"], Value::from(false));
    // Events are copilot's camelCase names; the shell string lands under `bash`.
    assert_eq!(root["hooks"]["sessionStart"][0]["type"], Value::from("command"));
    assert_eq!(root["hooks"]["sessionStart"][0]["bash"], Value::from("host_fixture self-heal"));
    assert_eq!(root["hooks"]["userPromptSubmitted"][0]["bash"], Value::from("host_fixture check-restart"));

    // A byte-identical re-render is a true NoOp.
    assert!(!reconcile_hooks(&path, &hooks).unwrap(), "second reconcile must be a NoOp");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn reconcile_hooks_skips_non_portable_and_remove_leaves_a_foreign_same_named_file() {
    let path = scratch("ez-fixture-plugin.json");
    let rooted = [hook("SessionStart", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh")];

    // A non-portable-only plugin never writes the file.
    assert!(!reconcile_hooks(&path, &rooted).unwrap(), "a non-portable hook must not be written");
    assert!(!path.exists(), "reconcile must not create a file for zero writable hooks");

    // A file already at our path (nothing we wrote) must survive remove when we have
    // no writable hooks — remove keys off the same set as the writer.
    std::fs::write(&path, "{\"not\":\"ours\"}").unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    assert!(!remove_hooks(&path, &rooted).unwrap(), "remove must not touch a file it never wrote");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "foreign same-named file survived byte-for-byte");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn remove_hooks_deletes_the_file_we_wrote() {
    let path = scratch("ez-fixture-plugin.json");
    let hooks = [hook("SessionStart", "host_fixture self-heal")];
    reconcile_hooks(&path, &hooks).unwrap();
    assert!(path.exists());
    assert!(remove_hooks(&path, &hooks).unwrap(), "remove must delete our owned file");
    assert!(!path.exists(), "our hooks file survived remove");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

// --- agents ------------------------------------------------------------------

#[test]
fn render_agent_emits_prefixed_name_and_quoted_description() {
    let doc = agent_doc();
    assert_eq!(agent_file("ez-fixture-plugin", &doc), "ez-fixture-plugin-ez-helper.agent.md");
    let md = render_agent("ez-fixture-plugin", &doc);
    assert!(md.contains(r#"name: "ez-fixture-plugin-ez-helper""#), "name must be plugin-prefixed and YAML-quoted:\n{md}");
    // The colon/quotes in the description stay inside a JSON-quoted (YAML-safe) scalar.
    assert!(md.contains(r#"description: "a: fixture \"agent\"""#), "description not YAML-safe:\n{md}");
    // `model` is not a documented copilot frontmatter field, so it is dropped.
    assert!(!md.contains("model:"), "model alias must not leak into the copilot agent file:\n{md}");
    assert!(md.trim_end().ends_with("Do the thing."), "body must be preserved:\n{md}");
}
