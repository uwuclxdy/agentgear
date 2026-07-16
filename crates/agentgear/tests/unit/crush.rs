//! crush backend unit tests: the one-file mcp+hooks reconcile shape, an idempotent
//! re-reconcile (NoOp), exact removal that preserves a pre-seeded user entry, the
//! `PreToolUse`-only event map, and the shared `${CLAUDE_PLUGIN_ROOT}` portability
//! filter for both servers and hooks (a non-portable entry is never written and so
//! must never be a removal candidate either).

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use super::mcpjson::{self, ServerShape};
use super::{BackendState, HookBinding, McpKind, McpServer};
use super::{hook_is_portable, map_event, portable_names, reconcile_config, remove_config};

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-crush-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn cleanup(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::remove_dir_all(parent).ok();
    }
}

fn read(path: &Path) -> Value {
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

fn pre_tool_hook(command: &str, matcher: Option<&str>) -> HookBinding {
    HookBinding { event: "PreToolUse".into(), matcher: matcher.map(str::to_string), command: command.into() }
}

#[test]
fn map_event_translates_only_pretooluse() {
    // Crush defines exactly one hook event; we fold casing (crush is case-insensitive)
    // but never invent an event for the other CC lifecycle names.
    assert_eq!(map_event("PreToolUse"), Some("PreToolUse"));
    assert_eq!(map_event("pretooluse"), Some("PreToolUse"));
    assert_eq!(map_event("PostToolUse"), None);
    assert_eq!(map_event("SessionStart"), None);
    assert_eq!(map_event("UserPromptSubmit"), None);
}

#[test]
fn portability_filters_claude_plugin_root() {
    let servers = [stdio("ez", "host_fixture", &["mcp"]), stdio("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/x", &[])];
    assert_eq!(portable_names(&servers), vec!["ez"]);
    assert!(hook_is_portable(&pre_tool_hook("guard.sh", None)));
    assert!(!hook_is_portable(&pre_tool_hook("${CLAUDE_PLUGIN_ROOT}/hooks/g.sh", None)));
}

#[test]
fn reconcile_writes_exact_shape_then_noops() {
    let path = scratch("crush.json");
    let servers = [stdio("ez-fixture", "host_fixture", &["mcp"])];
    let hooks = [pre_tool_hook("guard.sh", Some("^Bash$"))];

    let changed = reconcile_config(&path, &servers, &hooks).unwrap();
    assert!(changed, "first reconcile writes");

    let root = read(&path);
    // mcp: Typed shape under the root `mcp` map — crush requires an explicit `type`.
    assert_eq!(root["mcp"]["ez-fixture"], json!({ "type": "stdio", "command": "host_fixture", "args": ["mcp"], "env": {} }));
    // hooks: a flat `{command, matcher?}` entry under `hooks.PreToolUse`, same file.
    assert_eq!(root["hooks"]["PreToolUse"], json!([{ "command": "guard.sh", "matcher": "^Bash$" }]));

    let changed = reconcile_config(&path, &servers, &hooks).unwrap();
    assert!(!changed, "an unchanged reconcile must be a true NoOp (no write)");
    cleanup(&path);
}

#[test]
fn reconcile_skips_non_portable_and_unmapped_and_writes_no_empty_file() {
    let path = scratch("crush.json");
    let servers = [stdio("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/x", &[])];
    let hooks = [
        pre_tool_hook("${CLAUDE_PLUGIN_ROOT}/hooks/g.sh", None),
        HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() },
    ];

    let changed = reconcile_config(&path, &servers, &hooks).unwrap();
    assert!(!changed, "nothing writable -> no write");
    assert!(!path.exists(), "reconcile must not create an empty crush.json");
    cleanup(&path);
}

#[test]
fn mcp_only_plugin_does_not_create_a_hooks_key() {
    // The real fixture: one mcp server, no PreToolUse hook. Its non-PreToolUse hooks
    // must not conjure an empty `hooks` object.
    let path = scratch("crush.json");
    let servers = [stdio("ez-fixture", "host_fixture", &["mcp"])];
    let hooks = [HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() }];

    assert!(reconcile_config(&path, &servers, &hooks).unwrap());
    let root = read(&path);
    assert!(root["mcp"].get("ez-fixture").is_some(), "our mcp server landed");
    assert!(root.get("hooks").is_none(), "an mcp-only plugin must not create a `hooks` key");
    cleanup(&path);
}

#[test]
fn remove_deletes_only_ours_and_keeps_the_user_entry() {
    let path = scratch("crush.json");
    let seed = json!({
        "mcp": {
            "theirs": { "type": "stdio", "command": "their-server", "args": [], "env": {} },
            "ez-fixture": { "type": "stdio", "command": "host_fixture", "args": ["mcp"], "env": {} }
        },
        "hooks": {
            "PreToolUse": [
                { "command": "their-guard.sh" },
                { "command": "guard.sh", "matcher": "^Bash$" }
            ]
        }
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&seed).unwrap()).unwrap();

    let servers = [stdio("ez-fixture", "host_fixture", &["mcp"])];
    let hooks = [pre_tool_hook("guard.sh", Some("^Bash$"))];

    let changed = remove_config(&path, &portable_names(&servers), &hooks).unwrap();
    assert!(changed, "remove drops our entries");

    let root = read(&path);
    assert!(root["mcp"].get("ez-fixture").is_none(), "our mcp server survived remove");
    assert!(root["mcp"].get("theirs").is_some(), "remove clobbered the user's mcp server");
    let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 1, "only the user's hook should remain");
    assert_eq!(pre[0]["command"], "their-guard.sh");

    assert!(!remove_config(&path, &portable_names(&servers), &hooks).unwrap(), "a second remove is a NoOp");
    cleanup(&path);
}

#[test]
fn remove_never_touches_a_same_name_non_portable_survivor() {
    // A user hook whose command equals the literal (unexpanded) non-portable command
    // we would never have written must survive — we key removal off portable hooks only.
    let path = scratch("crush.json");
    let rooted = "${CLAUDE_PLUGIN_ROOT}/hooks/g.sh";
    let seed = json!({ "hooks": { "PreToolUse": [{ "command": rooted }] } });
    std::fs::write(&path, serde_json::to_vec_pretty(&seed).unwrap()).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    let changed = remove_config(&path, &[], &[pre_tool_hook(rooted, None)]).unwrap();
    assert!(!changed, "remove must not delete a hook it never wrote");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "the seeded hook survived byte-for-byte");
    cleanup(&path);
}

#[test]
fn remove_keeps_a_user_pretooluse_hook_matching_an_unmapped_owned_hook() {
    // A portable owned hook under an unmapped CC event (SessionStart) is never written
    // to crush.json, so a user's real PreToolUse hook sharing its command must survive:
    // remove keys off the same portable AND mapped-event filter reconcile writes with.
    let path = scratch("crush.json");
    let seed = json!({ "hooks": { "PreToolUse": [{ "command": "host_fixture hook" }] } });
    std::fs::write(&path, serde_json::to_vec_pretty(&seed).unwrap()).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    let owned = [HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture hook".into() }];
    let changed = remove_config(&path, &[], &owned).unwrap();
    assert!(!changed, "an unmapped owned hook must never sweep a user's PreToolUse entry");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "the user's hook survived byte-for-byte");
    cleanup(&path);
}

#[test]
fn remove_leaves_a_user_empty_array_under_an_unmanaged_event() {
    // A user-created empty array under an event we never manage must survive remove —
    // only an event array we actually emptied is dropped.
    let path = scratch("crush.json");
    let seed = json!({
        "mcp": { "ez-fixture": { "type": "stdio", "command": "host_fixture", "args": ["mcp"], "env": {} } },
        "hooks": { "PostToolUse": [] }
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&seed).unwrap()).unwrap();

    let servers = [stdio("ez-fixture", "host_fixture", &["mcp"])];
    let changed = remove_config(&path, &portable_names(&servers), &[]).unwrap();
    assert!(changed, "our mcp server is removed");
    let root = read(&path);
    assert!(root["mcp"].get("ez-fixture").is_none(), "our mcp server was removed");
    assert!(root["hooks"].get("PostToolUse").is_some(), "the user's empty PostToolUse array survived");
    cleanup(&path);
}

#[test]
fn probe_classifies_absent_healthy_needsrepair() {
    let path = scratch("crush.json");
    let servers = [stdio("ez-fixture", "host_fixture", &["mcp"])];

    // No file: none of our servers present -> Absent.
    assert!(matches!(mcpjson::probe(&path, &["mcp"], &servers, ServerShape::typed()).unwrap(), BackendState::Absent));

    reconcile_config(&path, &servers, &[]).unwrap();
    assert!(matches!(mcpjson::probe(&path, &["mcp"], &servers, ServerShape::typed()).unwrap(), BackendState::Healthy));

    // Drift the on-disk entry so it no longer matches our render -> NeedsRepair.
    let mut root = read(&path);
    root["mcp"]["ez-fixture"]["args"] = json!(["drifted"]);
    std::fs::write(&path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();
    assert!(matches!(mcpjson::probe(&path, &["mcp"], &servers, ServerShape::typed()).unwrap(), BackendState::NeedsRepair));
    cleanup(&path);
}
