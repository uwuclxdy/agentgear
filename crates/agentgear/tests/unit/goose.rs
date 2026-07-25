//! goose backend unit tests: the goose-specific YAML extension shape + idempotent
//! re-render, portability filtering for both mcp servers and hooks (a
//! `${CLAUDE_PLUGIN_ROOT}`-bearing entry is never written and so never a removal
//! candidate), the probe state classification, and the plugin-owned hooks dir
//! (wholesale write, wholesale drop). The hermetic fixture e2e never exercises the
//! non-portable path (the fixture plugin has no rooted entry), so it lives here.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use super::{hook_is_portable, probe_mcp, reconcile_hooks, reconcile_mcp, remove_mcp, remove_plugin_dir, writable_names};
use crate::agents::BackendState;
use crate::components::{HookBinding, McpKind, McpServer};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-goose-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn stdio(name: &str, command: &str, args: &[&str]) -> McpServer {
    McpServer {
        name: name.into(),
        kind: McpKind::Stdio,
        command: command.into(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
        env: BTreeMap::new(),
    }
}

#[test]
fn writable_names_excludes_rooted_and_sse_servers() {
    let sse = McpServer {
        name: "dead-sse".into(),
        kind: McpKind::Sse { url: "https://x/sse".into() },
        command: String::new(),
        args: vec![],
        env: BTreeMap::new(),
    };
    let servers = [stdio("ez-fixture", "host_fixture", &["mcp"]), stdio("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[]), sse];
    // sse is skipped like a non-portable server: goose runtime-refuses it.
    assert_eq!(writable_names(&servers), vec!["ez-fixture"]);
}

#[test]
fn hook_portability_matches_mcp_server_rule() {
    let portable = HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() };
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh".into() };
    assert!(hook_is_portable(&portable));
    assert!(!hook_is_portable(&rooted));
}

#[test]
fn reconcile_mcp_writes_goose_extension_shape_and_is_idempotent() {
    let config = scratch("config.yaml");
    // A foreign extension + an unrelated top-level key that MUST survive our merge.
    fs::write(&config, "GOOSE_MODEL: gpt-x\nextensions:\n  theirs:\n    type: stdio\n    cmd: their-server\n    enabled: true\n").unwrap();

    let srv = stdio("ez-fixture", "host_fixture", &["mcp"]);
    let servers = std::slice::from_ref(&srv);

    let changed = reconcile_mcp(&config, servers, true).unwrap();
    assert!(changed, "first reconcile must write");

    let text = fs::read_to_string(&config).unwrap();
    // goose's own field names (`cmd`/`type`/`enabled`/`timeout`), not CC's.
    assert!(text.contains("ez-fixture"), "our extension key missing:\n{text}");
    assert!(text.contains("cmd: host_fixture"), "goose `cmd` field missing:\n{text}");
    assert!(text.contains("type: stdio"), "goose `type` field missing:\n{text}");
    assert!(text.contains("- mcp"), "goose `args` entry missing:\n{text}");
    assert!(text.contains("enabled: true"), "goose `enabled` field missing:\n{text}");
    assert!(text.contains("timeout: 300"), "goose `timeout` field missing:\n{text}");
    // the seeded user config survived.
    assert!(text.contains("theirs") && text.contains("their-server"), "seeded extension was clobbered:\n{text}");
    assert!(text.contains("GOOSE_MODEL"), "seeded top-level key was clobbered:\n{text}");

    // idempotent: the parsed-back render equals a fresh render -> a true NoOp.
    let changed = reconcile_mcp(&config, servers, true).unwrap();
    assert!(!changed, "second identical reconcile must be a NoOp");

    // remove strips only ours; the seeded extension stays, and so does the mapping
    // holding it.
    let removed = remove_mcp(&config, &writable_names(servers)).unwrap();
    assert!(removed, "remove must report a change");
    let text = fs::read_to_string(&config).unwrap();
    assert!(!text.contains("ez-fixture"), "our extension survived remove:\n{text}");
    assert!(text.contains("their-server"), "remove clobbered the seeded extension:\n{text}");
    assert!(text.contains("extensions:"), "a mapping still holding theirs must stay:\n{text}");

    let _ = fs::remove_dir_all(config.parent().unwrap());
}

#[test]
fn remove_mcp_prunes_only_an_extensions_mapping_it_emptied() {
    let srv = stdio("ez-fixture", "host_fixture", &["mcp"]);
    let servers = std::slice::from_ref(&srv);

    // Ours were the only extensions: the mapping `reconcile_mcp` created goes with
    // them, leaving the user's own top-level key alone. The file always stays — a
    // YAML config can carry comments no removal could give back.
    let ours = scratch("config.yaml");
    fs::write(&ours, "GOOSE_MODEL: gpt-x\n").unwrap();
    reconcile_mcp(&ours, servers, true).unwrap();
    assert!(remove_mcp(&ours, &writable_names(servers)).unwrap(), "remove must report a change");
    assert_eq!(fs::read_to_string(&ours).unwrap(), "GOOSE_MODEL: gpt-x\n", "the extensions mapping our own removal emptied must go");

    // A mapping the user is keeping empty holds nothing of ours, so the teardown takes
    // nothing and writes nothing at all — the byte-for-byte file, comments included.
    let theirs = scratch("config.yaml");
    let seed = "# my goose config\nGOOSE_MODEL: gpt-x\nextensions: {}\n";
    fs::write(&theirs, seed).unwrap();
    assert!(!remove_mcp(&theirs, &writable_names(servers)).unwrap(), "a teardown with nothing of ours must report no change");
    assert_eq!(fs::read_to_string(&theirs).unwrap(), seed, "a mapping the user had empty must survive untouched");

    for p in [&ours, &theirs] {
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }
}

#[test]
fn non_portable_server_never_reaches_config_yaml() {
    let config = scratch("config.yaml");
    let servers = [stdio("ez-fixture", "host_fixture", &["mcp"]), stdio("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[])];

    let changed = reconcile_mcp(&config, &servers, true).unwrap();
    assert!(changed);
    let text = fs::read_to_string(&config).unwrap();
    assert!(text.contains("ez-fixture"), "portable server must be written:\n{text}");
    assert!(!text.contains("rooted"), "non-portable server leaked into config.yaml:\n{text}");

    // remove keys off the same filtered set, so it never targets a server it never wrote.
    let removed = remove_mcp(&config, &writable_names(&servers)).unwrap();
    assert!(removed);
    assert!(!fs::read_to_string(&config).unwrap().contains("ez-fixture"));

    let _ = fs::remove_dir_all(config.parent().unwrap());
}

#[test]
fn probe_mcp_classifies_absent_healthy_disabled_and_needs_repair() {
    let srv = stdio("ez-fixture", "host_fixture", &["mcp"]);
    let servers = std::slice::from_ref(&srv);

    // no file -> Absent.
    let missing = scratch("config.yaml");
    assert!(matches!(probe_mcp(&missing, servers).unwrap(), BackendState::Absent), "a missing config is Absent");

    // a matching render -> Healthy.
    let healthy = scratch("config.yaml");
    reconcile_mcp(&healthy, servers, true).unwrap();
    assert!(matches!(probe_mcp(&healthy, servers).unwrap(), BackendState::Healthy), "a matching extension is Healthy");

    // our exact render but `enabled: false` -> Disabled (a deliberate user disable).
    let disabled = scratch("config.yaml");
    reconcile_mcp(&disabled, servers, true).unwrap();
    fs::write(&disabled, fs::read_to_string(&disabled).unwrap().replace("enabled: true", "enabled: false")).unwrap();
    assert!(matches!(probe_mcp(&disabled, servers).unwrap(), BackendState::Disabled), "an enabled:false extension is Disabled");

    // a drifted field (hand-edited cmd) -> NeedsRepair.
    let drifted = scratch("config.yaml");
    reconcile_mcp(&drifted, servers, true).unwrap();
    fs::write(&drifted, fs::read_to_string(&drifted).unwrap().replace("cmd: host_fixture", "cmd: tampered")).unwrap();
    assert!(matches!(probe_mcp(&drifted, servers).unwrap(), BackendState::NeedsRepair), "a drifted extension is NeedsRepair");

    for p in [missing, healthy, disabled, drifted] {
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }
}

#[test]
fn probe_mcp_missing_config_is_healthy_when_no_portable_server() {
    // A hooks-only / rooted-mcp-only plugin writes no config.yaml, so a missing file
    // must be Healthy — Absent would make self_heal drop a correctly-installed marker.
    let missing = scratch("config.yaml");
    let rooted = [stdio("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[])];
    assert!(
        matches!(probe_mcp(&missing, &rooted).unwrap(), BackendState::Healthy),
        "a missing config with no portable server must be Healthy, not Absent"
    );
    assert!(matches!(probe_mcp(&missing, &[]).unwrap(), BackendState::Healthy), "a missing config with zero servers must be Healthy");

    let _ = fs::remove_dir_all(missing.parent().unwrap());
}

#[test]
fn reconcile_hooks_writes_the_owned_dir_skips_unmapped_events_and_remove_drops_it() {
    let root = std::env::temp_dir().join(format!("ez-goose-hooks-{:016x}", fastrand::u64(..)));
    let plugin_dir = root.join(".agents").join("plugins").join("ez-fixture-plugin");
    let hooks_json = plugin_dir.join("hooks").join("hooks.json");

    // A non-portable hook alone writes nothing and never creates the dir.
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/x.sh".into() };
    assert!(!reconcile_hooks(&hooks_json, std::slice::from_ref(&rooted)).unwrap(), "a non-portable hook must not be written");
    assert!(!hooks_json.exists(), "reconcile must not create a file for zero writable hooks");

    // Portable hooks under goose-named events land 1:1; an event goose does not
    // define (`PreCompact`) is dropped, never written under a guessed name.
    let hooks = vec![
        HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() },
        HookBinding { event: "UserPromptSubmit".into(), matcher: None, command: "host_fixture check-restart".into() },
        HookBinding { event: "PreCompact".into(), matcher: None, command: "host_fixture nope".into() },
    ];
    assert!(reconcile_hooks(&hooks_json, &hooks).unwrap(), "portable hooks must be written");
    let text = fs::read_to_string(&hooks_json).unwrap();
    assert!(text.contains("SessionStart") && text.contains("host_fixture self-heal"), "SessionStart hook missing:\n{text}");
    assert!(text.contains("UserPromptSubmit") && text.contains("host_fixture check-restart"), "UserPromptSubmit hook missing:\n{text}");
    assert!(!text.contains("PreCompact") && !text.contains("host_fixture nope"), "an unmapped event must be skipped:\n{text}");

    // wholesale render of a plugin-owned file -> a second write is a true NoOp.
    assert!(!reconcile_hooks(&hooks_json, &hooks).unwrap(), "an identical second hook write must be a NoOp");

    // remove drops the whole plugin dir; a second remove is a no-op.
    assert!(remove_plugin_dir(&plugin_dir).unwrap(), "remove must report the dir existed");
    assert!(!plugin_dir.exists(), "the plugin dir survived remove");
    assert!(!remove_plugin_dir(&plugin_dir).unwrap(), "removing an absent dir must be a no-op");

    let _ = fs::remove_dir_all(&root);
}
