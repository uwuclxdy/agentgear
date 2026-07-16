//! droid backend unit tests: the mcp write/probe/remove cycle against a temp
//! `mcp.json` (shared `mcpServers`/Plain shape at droid's key), CC-shape hook merge
//! into `hooks.json` with identity event mapping, portability filtering for both mcp
//! servers and hooks (a `${CLAUDE_PLUGIN_ROOT}`-bearing entry is never written and so
//! never a removal candidate), and the flat `<plugin>-<stem>.md` namespacing plus the
//! custom-droid `name` re-emit.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{doc_filename, hook_is_portable, map_event, namespaced, reconcile_hooks, remove_hooks, render_droid};
use crate::agents::BackendState;
use crate::agents::mcpjson::{self, ServerShape};
use crate::components::{HookBinding, MarkdownDoc, McpKind, McpServer};
use crate::host::Outcome;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-droid-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn read(path: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
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

// --- mcp: the shared renderer at droid's key + shape ------------------------

#[test]
fn mcp_reconcile_writes_plain_body_and_second_is_noop() {
    let path = scratch("mcp.json");
    let server = stdio("ez-fixture", "host_fixture", &["mcp"]);

    let out = mcpjson::reconcile(&path, &["mcpServers"], std::slice::from_ref(&server), ServerShape::plain()).unwrap();
    assert_eq!(out, Outcome::Installed);

    // Exact droid stdio shape: flat {command,args,env} under `mcpServers.<name>`, no `type`.
    let root = read(&path);
    assert_eq!(root["mcpServers"]["ez-fixture"], json!({ "command": "host_fixture", "args": ["mcp"], "env": {} }));

    // Idempotent: an unchanged reconcile does not rewrite the file.
    let out = mcpjson::reconcile(&path, &["mcpServers"], std::slice::from_ref(&server), ServerShape::plain()).unwrap();
    assert_eq!(out, Outcome::NoOp);

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mcp_probe_classifies_absent_healthy_and_needs_repair() {
    let path = scratch("mcp.json");
    let server = stdio("ez-fixture", "host_fixture", &["mcp"]);

    // No file yet -> Absent.
    assert!(matches!(
        mcpjson::probe(&path, &["mcpServers"], std::slice::from_ref(&server), ServerShape::plain()).unwrap(),
        BackendState::Absent
    ));

    mcpjson::reconcile(&path, &["mcpServers"], std::slice::from_ref(&server), ServerShape::plain()).unwrap();
    // Present and matching -> Healthy.
    assert!(matches!(
        mcpjson::probe(&path, &["mcpServers"], std::slice::from_ref(&server), ServerShape::plain()).unwrap(),
        BackendState::Healthy
    ));

    // Present but drifted (our key exists with a different body) -> NeedsRepair.
    let drifted = stdio("ez-fixture", "host_fixture", &["mcp", "--v2"]);
    assert!(matches!(
        mcpjson::probe(&path, &["mcpServers"], std::slice::from_ref(&drifted), ServerShape::plain()).unwrap(),
        BackendState::NeedsRepair
    ));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mcp_remove_deletes_only_ours_and_preserves_a_user_entry() {
    let path = scratch("mcp.json");
    // A user's own server + an unrelated top-level key that must both survive.
    std::fs::write(&path, r#"{"telemetry":false,"mcpServers":{"theirs":{"command":"their-server","args":[]}}}"#).unwrap();
    let server = stdio("ez-fixture", "host_fixture", &["mcp"]);

    mcpjson::reconcile(&path, &["mcpServers"], std::slice::from_ref(&server), ServerShape::plain()).unwrap();
    let root = read(&path);
    assert!(root["mcpServers"]["ez-fixture"].is_object(), "our server was not written");
    assert!(root["mcpServers"]["theirs"].is_object(), "the user's server was clobbered on reconcile");

    let out = mcpjson::remove(&path, &["mcpServers"], std::slice::from_ref(&server), ServerShape::plain()).unwrap();
    assert_eq!(out, Outcome::Removed);

    let root = read(&path);
    assert!(root["mcpServers"].get("ez-fixture").is_none(), "our server survived remove");
    assert_eq!(root["mcpServers"]["theirs"], json!({ "command": "their-server", "args": [] }), "user's server was touched");
    assert_eq!(root["telemetry"], json!(false), "unrelated top-level key was touched");

    // A second remove finds nothing of ours left -> NoOp.
    assert_eq!(mcpjson::remove(&path, &["mcpServers"], std::slice::from_ref(&server), ServerShape::plain()).unwrap(), Outcome::NoOp);

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

// --- hooks: CC-shape merge, identity event map, portability ------------------

#[test]
fn map_event_is_identity_for_the_full_cc_set_and_none_otherwise() {
    for e in [
        "PreToolUse",
        "PostToolUse",
        "UserPromptSubmit",
        "Notification",
        "Stop",
        "SubagentStop",
        "PreCompact",
        "SessionStart",
        "SessionEnd",
    ] {
        assert_eq!(map_event(e), Some(e), "droid hosts every CC event under its own name");
    }
    assert_eq!(map_event("PreCompress"), None, "an event droid does not host must be skipped, not guessed");
}

#[test]
fn reconcile_hooks_writes_cc_wrapper_shape_and_second_is_noop() {
    let path = scratch("hooks.json");
    let hook = HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() };

    let changed = reconcile_hooks(&path, std::slice::from_ref(&hook)).unwrap();
    assert!(changed, "first reconcile writes the hook");

    // Exact droid shape: `{hooks:{SessionStart:[{hooks:[{type,command}]}]}}`, no matcher.
    let root = read(&path);
    assert_eq!(root["hooks"]["SessionStart"], json!([{ "hooks": [{ "type": "command", "command": "host_fixture self-heal" }] }]));

    // Idempotent: the identical group is not re-added.
    let changed = reconcile_hooks(&path, std::slice::from_ref(&hook)).unwrap();
    assert!(!changed, "second reconcile of an already-present hook must be a NoOp");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn hook_portability_matches_mcp_server_rule() {
    let portable = HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() };
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh".into() };
    assert!(hook_is_portable(&portable));
    assert!(!hook_is_portable(&rooted));
}

#[test]
fn reconcile_hooks_skips_non_portable_and_remove_leaves_a_same_name_survivor() {
    let path = scratch("hooks.json");
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh".into() };

    // A non-portable hook is never written.
    let changed = reconcile_hooks(&path, std::slice::from_ref(&rooted)).unwrap();
    assert!(!changed, "a non-portable hook must not be written");
    assert!(!path.exists(), "reconcile_hooks must not create a file for zero writable hooks");

    // Seed the file with a user's own hook whose command equals the literal
    // (unexpanded) non-portable command we would have written — proves `remove_hooks`
    // never touches it, since we never wrote it.
    std::fs::write(&path, format!(r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}"}}]}}]}}}}"#, rooted.command))
        .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    let removed = remove_hooks(&path, std::slice::from_ref(&rooted)).unwrap();
    assert!(!removed, "remove_hooks must not remove a hook it never wrote");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "seeded hook survived byte-for-byte");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

// --- commands / droids namespacing + re-emit ---------------------------------

#[test]
fn doc_filename_flattens_nested_paths_and_prefixes_the_plugin() {
    assert_eq!(doc_filename("ez", "commands/hello.md", "commands/"), "ez-hello.md");
    assert_eq!(doc_filename("ez", "commands/sub/foo.md", "commands/"), "ez-sub-foo.md");
    assert_eq!(doc_filename("ez", "agents/helper.md", "agents/"), "ez-helper.md");
    assert_eq!(namespaced("ez", "agents/helper.md", "agents/"), "ez-helper");
}

#[test]
fn render_droid_namespaces_name_and_passes_frontmatter_through() {
    let mut frontmatter = BTreeMap::new();
    frontmatter.insert("name".to_string(), Value::from("helper"));
    frontmatter.insert("description".to_string(), Value::from("a fixture helper"));
    frontmatter.insert("model".to_string(), Value::from("sonnet"));
    let doc =
        MarkdownDoc { name: "helper".into(), rel: "agents/helper.md".into(), frontmatter, body: "Do the thing.".into(), raw: Vec::new() };

    let rendered = render_droid("myplug", &doc.rel, &doc);
    assert!(rendered.contains("name: myplug-helper"), "custom-droid name must be plugin-namespaced:\n{rendered}");
    assert!(!rendered.contains("name: helper\n"), "the original bare name must be overridden:\n{rendered}");
    assert!(rendered.contains("model: sonnet"), "model frontmatter must pass through:\n{rendered}");
    assert!(rendered.contains("description: a fixture helper"), "description frontmatter must pass through:\n{rendered}");
    assert!(rendered.contains("Do the thing."), "the body must be carried into the system prompt:\n{rendered}");

    // Deterministic: a re-render is byte-identical (drives write_file_idem NoOp).
    assert_eq!(rendered, render_droid("myplug", &doc.rel, &doc));
}
