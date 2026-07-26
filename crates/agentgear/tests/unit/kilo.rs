//! kilo backend unit tests: the opencode-shaped mcp reconcile/probe/remove on a
//! throwaway `kilo.json` — exact written shape, a true second-reconcile `NoOp`,
//! merge-safe removal, probe classification, and the `${CLAUDE_PLUGIN_ROOT}`
//! portability filter (a non-portable server is never written, so `remove` keying
//! off the same filtered set can never delete a same-named user entry).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{BackendState, portable_names, probe_mcp, reconcile_mcp, remove_mcp, render_mcp_server};
use crate::components::{McpKind, McpServer};

/// A fresh unique config path per call, so cargo's parallel test threads never
/// share a `kilo.json` (mirrors the codex/gemini unit-test scratch helpers).
fn scratch() -> PathBuf {
    let dir = crate::scratch::path("ez-kilo-unit");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("kilo.json")
}

fn server(name: &str, command: &str, args: &[&str]) -> McpServer {
    McpServer {
        name: name.into(),
        kind: McpKind::Stdio,
        command: command.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        env: BTreeMap::new(),
    }
}

fn read(path: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn portable_names_excludes_claude_plugin_root_servers() {
    let servers = [server("ez-fixture", "host_fixture", &["mcp"]), server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[])];
    assert_eq!(portable_names(&servers), vec!["ez-fixture"]);
}

#[test]
fn reconcile_writes_the_opencode_shape_and_second_run_is_a_noop() {
    let path = scratch();
    let servers = [server("ez-fixture", "host_fixture", &["mcp"])];

    // First reconcile writes; the merged `command` array (not CC's split
    // command/args), `environment` (not `env`), and `type:"local"` are the
    // load-bearing fork-specific differences from the CC `mcpServers` family.
    assert!(reconcile_mcp(&path, &servers, true).unwrap(), "first reconcile must write");
    let root = read(&path);
    assert_eq!(
        root["mcp"]["ez-fixture"],
        json!({ "type": "local", "command": ["host_fixture", "mcp"], "environment": {}, "enabled": true }),
        "written server body drifted from the kilo/opencode shape:\n{root:#}",
    );

    // Second identical reconcile is byte-identical -> a true NoOp (no write).
    assert!(!reconcile_mcp(&path, &servers, true).unwrap(), "an already-converged reconcile must not write");
}

#[test]
fn reconcile_skips_non_portable_and_leaves_the_user_config_untouched() {
    let path = scratch();
    // Seed a foreign server + an unrelated top-level key that must both survive.
    std::fs::write(&path, r#"{ "theme": "dark", "mcp": { "theirs": { "type": "local", "command": ["their-server"], "enabled": true } } }"#)
        .unwrap();

    let servers = [server("ez-fixture", "host_fixture", &["mcp"]), server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[])];
    assert!(reconcile_mcp(&path, &servers, true).unwrap(), "reconcile must add our portable server");
    let root = read(&path);
    assert!(root["mcp"].get("ez-fixture").is_some(), "portable server missing:\n{root:#}");
    assert!(root["mcp"].get("rooted").is_none(), "non-portable server leaked into kilo.json:\n{root:#}");
    assert_eq!(root["theme"], json!("dark"), "unrelated top-level key was clobbered:\n{root:#}");
    assert!(root["mcp"].get("theirs").is_some(), "seeded user server was clobbered:\n{root:#}");
}

#[test]
fn remove_deletes_only_ours_and_never_a_same_named_user_entry() {
    let path = scratch();
    let servers = [server("ez-fixture", "host_fixture", &["mcp"]), server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[])];

    // A user server literally named "rooted" (the same name as our non-portable,
    // never-written one) must survive: `remove` keys off `portable_names`, which
    // excludes it, so it is never a deletion target.
    std::fs::write(&path, r#"{ "mcp": { "rooted": { "type": "local", "command": ["users-own"], "enabled": true } } }"#).unwrap();
    reconcile_mcp(&path, &servers, true).unwrap();

    assert!(remove_mcp(&path, &portable_names(&servers)).unwrap(), "remove must delete our written server");
    let root = read(&path);
    assert!(root["mcp"].get("ez-fixture").is_none(), "our server survived remove:\n{root:#}");
    assert_eq!(
        root["mcp"]["rooted"],
        json!({ "type": "local", "command": ["users-own"], "enabled": true }),
        "a same-named user entry we never wrote was deleted:\n{root:#}",
    );
}

#[test]
fn probe_classifies_absent_healthy_and_needs_repair() {
    let path = scratch();
    let servers = [server("ez-fixture", "host_fixture", &["mcp"])];

    // Missing file -> Absent.
    assert!(matches!(probe_mcp(&path, &servers).unwrap(), BackendState::Absent), "a missing config must probe Absent");

    // Present but our server absent (only a foreign one) -> Absent.
    std::fs::write(&path, r#"{ "mcp": { "theirs": { "type": "local", "command": ["their-server"], "enabled": true } } }"#).unwrap();
    assert!(matches!(probe_mcp(&path, &servers).unwrap(), BackendState::Absent), "our server absent must probe Absent");

    // Our server present + matching the enabled render -> Healthy.
    reconcile_mcp(&path, &servers, true).unwrap();
    assert!(matches!(probe_mcp(&path, &servers).unwrap(), BackendState::Healthy), "a converged install must probe Healthy");

    // Our key present but drifted (a different command) -> NeedsRepair.
    let mut root = read(&path);
    root["mcp"]["ez-fixture"] = json!({ "type": "local", "command": ["stale-binary"], "environment": {}, "enabled": true });
    std::fs::write(&path, serde_json::to_vec(&root).unwrap()).unwrap();
    assert!(matches!(probe_mcp(&path, &servers).unwrap(), BackendState::NeedsRepair), "a drifted install must probe NeedsRepair");
}

#[test]
fn probe_is_healthy_never_absent_for_a_plugin_with_no_portable_servers() {
    // A plugin whose only mcp server is non-portable declares nothing we own; probe
    // must return Healthy (not Absent) so self_heal never drops a present marker.
    let path = scratch();
    let servers = [server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[])];
    assert!(!path.exists());
    assert!(matches!(probe_mcp(&path, &servers).unwrap(), BackendState::Healthy), "no portable servers must probe Healthy, not Absent");
}

#[test]
fn self_heal_never_reenables_a_user_disabled_server() {
    let path = scratch();
    let servers = [server("ez-fixture", "host_fixture", &["mcp"])];
    reconcile_mcp(&path, &servers, true).unwrap();

    // The user disables our server through kilo's own `enabled` flag.
    let mut root = read(&path);
    root["mcp"]["ez-fixture"] = render_mcp_server(&servers[0], false);
    std::fs::write(&path, serde_json::to_vec(&root).unwrap()).unwrap();
    assert!(matches!(probe_mcp(&path, &servers).unwrap(), BackendState::Disabled), "a user-disabled server must probe Disabled");

    // self_heal (reenable=false) leaves the deliberate disable in place -> NoOp.
    assert!(!reconcile_mcp(&path, &servers, false).unwrap(), "self_heal must not re-enable a user disable");
    assert_eq!(read(&path)["mcp"]["ez-fixture"]["enabled"], json!(false), "self_heal flipped a user disable back on");

    // An explicit install/update (reenable=true) honors the user's request.
    assert!(reconcile_mcp(&path, &servers, true).unwrap(), "explicit reconcile must re-enable");
    assert_eq!(read(&path)["mcp"]["ez-fixture"]["enabled"], json!(true), "explicit reconcile did not re-enable");
}
