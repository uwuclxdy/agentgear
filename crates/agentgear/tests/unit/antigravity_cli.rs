//! antigravity-cli backend unit tests. Two ownership models are exercised: the
//! shared json `mcpServers` renderer (as the backend wires it — Plain shape, at the
//! `mcp_config.json` the desktop antigravity backend also writes) and the bespoke
//! plugin-name-keyed hooks tree. Both must write an exact shape, no-op on a second
//! reconcile, remove only what we own, and skip `${CLAUDE_PLUGIN_ROOT}` entries.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{hook_is_portable, hooks_path, map_event, mcp_path, portable_names, reconcile_hooks, remove_hooks};
use crate::agents::BackendState;
use crate::agents::mcpjson::{self, ServerShape};
use crate::components::{HookBinding, McpKind, McpServer};
use crate::host::{Outcome, Scope};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-antigravity-cli-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str, args: &[&str]) -> McpServer {
    McpServer {
        name: name.into(),
        kind: McpKind::Stdio,
        command: command.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        env: BTreeMap::new(),
    }
}

fn hook(event: &str, command: &str) -> HookBinding {
    HookBinding { event: event.into(), matcher: None, command: command.into() }
}

// --- paths -------------------------------------------------------------------

#[test]
fn project_paths_use_the_agents_dir_but_mcp_and_hooks_split_at_user_scope() {
    // Project scope: both live under the workspace's native `.agents/` dir.
    let root = PathBuf::from("/work/repo");
    let scope = Scope::Project { path: root.clone() };
    assert_eq!(mcp_path(&scope).unwrap(), root.join(".agents").join("mcp_config.json"));
    assert_eq!(hooks_path(&scope).unwrap(), root.join(".agents").join("hooks.json"));

    // User scope: mcp is the SHARED `config/` file, hooks the CLI-specific dir —
    // they intentionally diverge, so a single base-join would be wrong.
    let mcp = mcp_path(&Scope::User).unwrap();
    let hooks = hooks_path(&Scope::User).unwrap();
    assert!(mcp.ends_with("config/mcp_config.json"), "user mcp path: {}", mcp.display());
    assert!(hooks.ends_with("antigravity-cli/hooks.json"), "user hooks path: {}", hooks.display());
    assert_ne!(mcp.parent(), hooks.parent(), "user mcp and hooks must not share a dir");
}

// --- mcp (shared renderer, wired exactly as the backend does) ----------------

#[test]
fn mcp_reconcile_writes_plain_shape_then_noops_and_remove_keeps_the_user_entry() {
    let path = scratch("mcp_config.json");
    // Seed a foreign server that must survive install + uninstall.
    std::fs::write(&path, r#"{"mcpServers":{"theirs":{"command":"their-bin","args":[]}}}"#).unwrap();

    let ours = [server("ez-fixture", "host_fixture", &["mcp"])];

    // Plain shape = `{command, args, env}`, no `type` — the shape the desktop
    // antigravity backend also emits, so a cross-backend re-write is a NoOp.
    let out = mcpjson::reconcile(&path, &["mcpServers"], &ours, ServerShape::Plain).unwrap();
    assert_eq!(out, Outcome::Installed);
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["mcpServers"]["ez-fixture"], json!({ "command": "host_fixture", "args": ["mcp"], "env": {} }));
    assert_eq!(v["mcpServers"]["theirs"], json!({ "command": "their-bin", "args": [] }), "seeded server clobbered");

    // idempotent second reconcile.
    assert_eq!(mcpjson::reconcile(&path, &["mcpServers"], &ours, ServerShape::Plain).unwrap(), Outcome::NoOp);

    // remove strips only ours (portable names), leaving the user's.
    let names = portable_names(&ours);
    assert_eq!(mcpjson::remove(&path, &["mcpServers"], &names).unwrap(), Outcome::Removed);
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(v["mcpServers"].get("ez-fixture").is_none(), "our server survived remove");
    assert!(v["mcpServers"].get("theirs").is_some(), "remove clobbered the seeded server");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mcp_probe_classifies_absent_healthy_and_needs_repair() {
    let path = scratch("mcp_config.json");
    let ours = [server("ez-fixture", "host_fixture", &["mcp"])];

    // Missing file -> Absent.
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &ours, ServerShape::Plain).unwrap(), BackendState::Absent));

    // Present + matching -> Healthy.
    mcpjson::reconcile(&path, &["mcpServers"], &ours, ServerShape::Plain).unwrap();
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &ours, ServerShape::Plain).unwrap(), BackendState::Healthy));

    // Present but drifted (a hand-edited command) -> NeedsRepair.
    std::fs::write(&path, r#"{"mcpServers":{"ez-fixture":{"command":"tampered","args":[],"env":{}}}}"#).unwrap();
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &ours, ServerShape::Plain).unwrap(), BackendState::NeedsRepair));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn portable_names_excludes_claude_plugin_root_servers() {
    let servers = [server("ez-fixture", "host_fixture", &["mcp"]), server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[])];
    assert_eq!(portable_names(&servers), vec!["ez-fixture"]);
}

// --- hooks (bespoke plugin-name-keyed tree) ----------------------------------

#[test]
fn event_map_covers_the_briefs_names_and_skips_the_rest() {
    assert_eq!(map_event("SessionStart"), Some("SessionStart"));
    assert_eq!(map_event("UserPromptSubmit"), Some("BeforeAgent"));
    assert_eq!(map_event("PreToolUse"), Some("PreToolUse"));
    assert_eq!(map_event("PostToolUse"), Some("PostToolUse"));
    assert_eq!(map_event("Stop"), Some("Stop"));
    // No listed Antigravity analog -> skipped, never guessed.
    for unmapped in ["SessionEnd", "SubagentStop", "PreCompact", "Notification"] {
        assert_eq!(map_event(unmapped), None, "{unmapped} should be skipped");
    }
}

#[test]
fn hook_portability_matches_the_mcp_rule() {
    assert!(hook_is_portable(&hook("SessionStart", "host_fixture self-heal")));
    assert!(!hook_is_portable(&hook("SessionStart", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh")));
}

#[test]
fn reconcile_hooks_writes_the_plugin_keyed_tree_then_noops() {
    let path = scratch("hooks.json");
    let hooks = [hook("SessionStart", "host_fixture self-heal"), hook("UserPromptSubmit", "host_fixture check-restart")];

    assert!(reconcile_hooks(&path, "ez-fixture-plugin", &hooks).unwrap());
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    // Exact shape: our top-level plugin key, a FLAT `{type, command}` entry (matcher
    // omitted when None) under each mapped event — the shape `agy` reads, not the
    // CC-nested `{hooks:[...]}` group.
    assert_eq!(v["ez-fixture-plugin"]["SessionStart"], json!([{ "type": "command", "command": "host_fixture self-heal" }]));
    assert_eq!(
        v["ez-fixture-plugin"]["BeforeAgent"],
        json!([{ "type": "command", "command": "host_fixture check-restart" }]),
        "UserPromptSubmit was not mapped to BeforeAgent"
    );

    // idempotent: a rebuilt-identical subtree is a true NoOp (no write).
    assert!(!reconcile_hooks(&path, "ez-fixture-plugin", &hooks).unwrap(), "second reconcile should no-op");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn reconcile_hooks_writes_matcher_as_a_flat_sibling() {
    let path = scratch("hooks.json");
    // A matcher must land as a sibling of type/command in the flat entry, never
    // inside a nested CC `{hooks:[...]}` group.
    let matched = HookBinding { event: "PreToolUse".into(), matcher: Some("Bash".into()), command: "host_fixture guard".into() };

    assert!(reconcile_hooks(&path, "ez-fixture-plugin", std::slice::from_ref(&matched)).unwrap());
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["ez-fixture-plugin"]["PreToolUse"], json!([{ "matcher": "Bash", "type": "command", "command": "host_fixture guard" }]));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn reconcile_hooks_skips_non_portable_and_writes_no_file() {
    let path = scratch("hooks.json");
    let rooted = hook("SessionStart", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh");

    assert!(!reconcile_hooks(&path, "ez-fixture-plugin", std::slice::from_ref(&rooted)).unwrap());
    assert!(!path.exists(), "a non-portable-only hook set must not create hooks.json");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn remove_hooks_deletes_only_our_key_and_keeps_a_foreign_plugin() {
    let path = scratch("hooks.json");
    // Seed a foreign plugin's hooks under its OWN top-level key; our own key carries
    // the flat entry shape the backend writes.
    std::fs::write(
        &path,
        r#"{"other-plugin":{"SessionStart":[{"type":"command","command":"their-hook"}]},"ez-fixture-plugin":{"Stop":[{"type":"command","command":"host_fixture bye"}]}}"#,
    )
    .unwrap();

    assert!(remove_hooks(&path, "ez-fixture-plugin").unwrap());
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(v.get("ez-fixture-plugin").is_none(), "our hook key survived remove");
    assert!(v.get("other-plugin").is_some(), "remove clobbered a foreign plugin's hooks");

    // Removing again is a NoOp (our key already gone).
    assert!(!remove_hooks(&path, "ez-fixture-plugin").unwrap(), "second remove should no-op");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
