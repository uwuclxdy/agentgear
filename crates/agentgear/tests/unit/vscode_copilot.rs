//! vscode-copilot backend unit tests. The backend forwards mcp to the shared json
//! renderer with its own key (`servers`, not `mcpServers`) and the Typed
//! (`{type:"stdio",...}`) shape, owns its whole hooks file (`<plugin>.json`), and
//! renders CC agent defs into `.agent.md`. The tests drive those seams directly:
//! exact mcp shape, idempotent NoOp, user-entry-preserving removal, probe
//! classification, the `${CLAUDE_PLUGIN_ROOT}` portability skip (mcp + hooks), the
//! event map (identity + skipping an unmapped event), and the project-scope guard.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{agent_file, hook_is_portable, map_event, portable_names, project_root, reconcile_hooks, render_agent};
use crate::agents::{BackendState, mcpjson};
use crate::components::{HookBinding, MarkdownDoc, McpKind, McpServer};
use crate::host::{Outcome, Scope};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-vscodecopilot-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: vec!["mcp".into()], env: BTreeMap::new() }
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn reconcile_writes_servers_key_with_typed_stdio_shape() {
    let path = scratch("mcp.json");
    let servers = [server("ez-fixture", "host_fixture")];

    let outcome = mcpjson::reconcile(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap();
    assert_eq!(outcome, Outcome::Installed);

    let root = read(&path);
    // Root key is `servers`, NOT CC's `mcpServers`.
    assert!(root.get("mcpServers").is_none(), "must not write the `mcpServers` key:\n{root}");
    let entry = &root["servers"]["ez-fixture"];
    assert_eq!(entry["type"], "stdio", "stdio entries carry an explicit type:\n{entry}");
    assert_eq!(entry["command"], "host_fixture");
    assert_eq!(entry["args"][0], "mcp");
    assert!(entry.get("env").is_some(), "env object present:\n{entry}");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn second_reconcile_is_noop() {
    let path = scratch("mcp.json");
    let servers = [server("ez-fixture", "host_fixture")];

    assert_eq!(mcpjson::reconcile(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap(), Outcome::Installed);
    assert_eq!(
        mcpjson::reconcile(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap(),
        Outcome::NoOp,
        "a converged reconcile must not rewrite the file"
    );

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn remove_deletes_only_ours_and_keeps_a_pre_seeded_user_entry() {
    let path = scratch("mcp.json");
    // A foreign server + an unrelated sibling key (VS Code's `inputs`) that must
    // survive our whole lifecycle.
    std::fs::write(&path, r#"{"inputs":[],"servers":{"theirs":{"type":"stdio","command":"their-server","args":[],"env":{}}}}"#).unwrap();

    let servers = [server("ez-fixture", "host_fixture")];
    mcpjson::reconcile(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap();

    let removed = mcpjson::remove(&path, super::MCP_KEY, &portable_names(&servers)).unwrap();
    assert_eq!(removed, Outcome::Removed);

    let root = read(&path);
    assert!(root["servers"].get("ez-fixture").is_none(), "our server survived removal:\n{root}");
    assert_eq!(root["servers"]["theirs"]["command"], "their-server", "seeded user server was clobbered:\n{root}");
    assert!(root.get("inputs").is_some(), "unrelated sibling key was dropped:\n{root}");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn probe_classifies_absent_healthy_and_needs_repair() {
    let path = scratch("mcp.json");
    let servers = [server("ez-fixture", "host_fixture")];

    // No file yet -> Absent.
    assert!(matches!(mcpjson::probe(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap(), BackendState::Absent));

    // Written and matching -> Healthy.
    mcpjson::reconcile(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap();
    assert!(matches!(mcpjson::probe(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap(), BackendState::Healthy));

    // Same key present but the desired body drifted -> NeedsRepair.
    let drifted = [server("ez-fixture", "different-binary")];
    assert!(matches!(mcpjson::probe(&path, super::MCP_KEY, &drifted, super::SHAPE).unwrap(), BackendState::NeedsRepair));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn non_portable_claude_plugin_root_server_is_skipped() {
    let path = scratch("mcp.json");
    let leaky = McpServer {
        name: "rooted".into(),
        kind: McpKind::Stdio,
        command: "${CLAUDE_PLUGIN_ROOT}/bin/leaky".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    let servers = [server("ez-fixture", "host_fixture"), leaky.clone()];

    // The portable server is written; the non-portable one never reaches mcp.json.
    mcpjson::reconcile(&path, super::MCP_KEY, &servers, super::SHAPE).unwrap();
    let root = read(&path);
    assert!(root["servers"].get("ez-fixture").is_some(), "portable server must be written:\n{root}");
    assert!(root["servers"].get("rooted").is_none(), "non-portable server leaked into mcp.json:\n{root}");

    // remove keys off the same filter, so the non-portable server is never a
    // removal candidate (an unfiltered name could delete a same-named user entry).
    assert_eq!(portable_names(&servers), vec!["ez-fixture"]);
    assert!(portable_names(std::slice::from_ref(&leaky)).is_empty());

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn hook_portability_and_event_mapping() {
    let portable = HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() };
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh".into() };
    assert!(hook_is_portable(&portable));
    assert!(!hook_is_portable(&rooted));

    // VS Code shares CC's PascalCase names for the events it has -> identity map.
    assert_eq!(map_event("SessionStart"), Some("SessionStart"));
    assert_eq!(map_event("UserPromptSubmit"), Some("UserPromptSubmit"));
    assert_eq!(map_event("PreToolUse"), Some("PreToolUse"));
    assert_eq!(map_event("SubagentStop"), Some("SubagentStop"));
    // CC-only among the overlap set -> no analog, skipped (never guessed).
    assert_eq!(map_event("SessionEnd"), None);
    assert_eq!(map_event("Notification"), None);
}

#[test]
fn reconcile_hooks_writes_owned_file_maps_events_and_is_idempotent() {
    let path = scratch("ez-fixture-plugin.json");
    let hooks = vec![
        HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() },
        HookBinding { event: "UserPromptSubmit".into(), matcher: None, command: "host_fixture check-restart".into() },
        HookBinding { event: "PreToolUse".into(), matcher: Some("Write".into()), command: "host_fixture guard".into() },
        // no VS Code analog -> skipped
        HookBinding { event: "SessionEnd".into(), matcher: None, command: "host_fixture bye".into() },
        // non-portable -> skipped
        HookBinding { event: "Stop".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/x.sh".into() },
    ];

    assert!(reconcile_hooks(&path, &hooks).unwrap(), "first write must report a change");
    let root = read(&path);
    let events = root["hooks"].as_object().unwrap();
    assert!(events.contains_key("SessionStart") && events.contains_key("UserPromptSubmit") && events.contains_key("PreToolUse"));
    assert!(!events.contains_key("SessionEnd"), "an event with no VS Code analog must be skipped:\n{root}");
    assert!(!events.contains_key("Stop"), "a non-portable hook must be skipped:\n{root}");
    // entry shape: flat command object; CC's matcher is dropped (the hook above carries
    // `matcher: Some("Write")`, but VS Code's native schema has no matcher field).
    let pre = &events["PreToolUse"][0];
    assert_eq!(pre["type"], "command");
    assert_eq!(pre["command"], "host_fixture guard");
    assert!(pre.get("matcher").is_none(), "CC matcher must not leak into the VS Code entry:\n{root}");

    // idempotent: identical hooks -> no rewrite.
    assert!(!reconcile_hooks(&path, &hooks).unwrap(), "a converged hooks file must not be rewritten");

    // the file is entirely ours: dropping every writable hook removes it.
    assert!(reconcile_hooks(&path, &hooks[3..]).unwrap(), "removing all writable hooks must change (delete) the owned file");
    assert!(!path.exists(), "owned hooks file must be gone when no writable hook remains");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn reconcile_hooks_creates_nothing_for_zero_writable_hooks() {
    let path = scratch("ez-fixture-plugin.json");
    let hooks = vec![
        HookBinding { event: "SessionEnd".into(), matcher: None, command: "host_fixture bye".into() },
        HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/x.sh".into() },
    ];
    assert!(!reconcile_hooks(&path, &hooks).unwrap(), "no writable hook must not report a change");
    assert!(!path.exists(), "reconcile_hooks must not create a file for zero writable hooks");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn render_agent_name_matches_filename_stem_even_when_frontmatter_name_differs() {
    let mut fm = BTreeMap::new();
    // Frontmatter `name` deliberately differs from the file stem (`ez-helper`): the
    // rendered name must follow the stem so it matches the `.agent.md` filename our
    // `remove`/`doctor` key on, never diverging from the CC `name` field.
    fm.insert("name".to_string(), Value::String("helper".into()));
    fm.insert("description".to_string(), Value::String("does things: carefully".into()));
    let doc = MarkdownDoc {
        name: "ez-helper".into(),
        rel: "agents/ez-helper.md".into(),
        frontmatter: fm,
        body: "You are a helper.\n".into(),
        raw: Vec::new(),
    };

    assert_eq!(agent_file("ez-fixture-plugin", &doc), "ez-fixture-plugin-ez-helper.agent.md");

    let rendered = render_agent("ez-fixture-plugin", &doc);
    assert!(
        rendered.contains("name: ez-fixture-plugin-ez-helper"),
        "frontmatter name must be the plugin-prefixed file stem, not the CC `name` field:\n{rendered}"
    );
    // JSON-quoted description keeps the colon from breaking the YAML scalar.
    assert!(rendered.contains(r#"description: "does things: carefully""#), "description must be JSON-quoted:\n{rendered}");
    assert!(!rendered.contains("model:"), "CC model alias must be dropped:\n{rendered}");
    assert!(rendered.trim_end().ends_with("You are a helper."), "body must follow the frontmatter:\n{rendered}");
}

#[test]
fn project_root_rejects_user_scope() {
    assert!(project_root(&Scope::User).is_err(), "user scope has no config surface for a project-only backend");
    let dir = std::env::temp_dir();
    assert_eq!(project_root(&Scope::Project { path: dir.clone() }).unwrap(), dir.as_path());
}
