//! antigravity-cli backend unit tests. Two ownership models are exercised: the
//! shared json `mcpServers` renderer (as the backend wires it — Plain shape, at the
//! `mcp_config.json` the desktop antigravity backend also writes) and the bespoke
//! plugin-name-keyed hooks tree. Both must write an exact shape, no-op on a second
//! reconcile, remove only what we own, and skip `${CLAUDE_PLUGIN_ROOT}` entries.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{hook_is_portable, hooks_path, map_event, mcp_path, probe_hooks, reconcile_hooks, remove_hooks, render_hook_tree};
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
fn both_configs_live_in_the_scope_s_customization_root() {
    // Project scope: both live under the workspace's native `.agents/` dir.
    let root = PathBuf::from("/work/repo");
    let scope = Scope::Project { path: root.clone() };
    assert_eq!(mcp_path(&scope).unwrap(), root.join(".agents").join("mcp_config.json"));
    assert_eq!(hooks_path(&scope).unwrap(), root.join(".agents").join("hooks.json"));

    // User scope: `~/.gemini/config/` is the global customization root, and `agy`
    // scans a root for both files. `~/.gemini/antigravity-cli/` holds the CLI's own
    // settings + transcripts and is scanned for neither, so a hooks.json there is
    // never loaded (Google fixed this same bug in their own TUI, CHANGELOG v1.0.8).
    let mcp = mcp_path(&Scope::User).unwrap();
    let hooks = hooks_path(&Scope::User).unwrap();
    assert!(mcp.ends_with("config/mcp_config.json"), "user mcp path: {}", mcp.display());
    assert!(hooks.ends_with("config/hooks.json"), "user hooks path: {}", hooks.display());
    assert_eq!(mcp.parent(), hooks.parent(), "both user files live in the one customization root");
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
    let out = mcpjson::reconcile(&path, &["mcpServers"], &ours, ServerShape::plain()).unwrap();
    assert_eq!(out, Outcome::Installed);
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["mcpServers"]["ez-fixture"], json!({ "command": "host_fixture", "args": ["mcp"], "env": {} }));
    assert_eq!(v["mcpServers"]["theirs"], json!({ "command": "their-bin", "args": [] }), "seeded server clobbered");

    // idempotent second reconcile.
    assert_eq!(mcpjson::reconcile(&path, &["mcpServers"], &ours, ServerShape::plain()).unwrap(), Outcome::NoOp);

    // remove strips only ours (the same writable filter reconcile uses), leaving the user's.
    assert_eq!(mcpjson::remove(&path, &["mcpServers"], &ours, ServerShape::plain()).unwrap(), Outcome::Removed);
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
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &ours, ServerShape::plain()).unwrap(), BackendState::Absent));

    // Present + matching -> Healthy.
    mcpjson::reconcile(&path, &["mcpServers"], &ours, ServerShape::plain()).unwrap();
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &ours, ServerShape::plain()).unwrap(), BackendState::Healthy));

    // Present but drifted (a hand-edited command) -> NeedsRepair.
    std::fs::write(&path, r#"{"mcpServers":{"ez-fixture":{"command":"tampered","args":[],"env":{}}}}"#).unwrap();
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &ours, ServerShape::plain()).unwrap(), BackendState::NeedsRepair));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

// --- hooks (bespoke plugin-name-keyed tree) ----------------------------------

#[test]
fn event_map_emits_only_agy_s_five_legal_events() {
    // `agy` documents exactly five. Anything else is written and silently ignored,
    // so a name outside this set is worse than a skip: it looks wired and never fires.
    const LEGAL: [&str; 5] = ["PreToolUse", "PostToolUse", "PreInvocation", "PostInvocation", "Stop"];
    for cc in ["SessionStart", "SessionEnd", "UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop", "SubagentStop", "PreCompact"] {
        if let Some(mapped) = map_event(cc) {
            assert!(LEGAL.contains(&mapped), "{cc} maps to `{mapped}`, which is not a legal agy event");
        }
    }

    assert_eq!(map_event("PreToolUse"), Some("PreToolUse"));
    assert_eq!(map_event("PostToolUse"), Some("PostToolUse"));
    assert_eq!(map_event("Stop"), Some("Stop"));
    // `PreInvocation` fires before the model runs: agy's own "before the agent loop"
    // analog, and the closest thing to CC's UserPromptSubmit.
    assert_eq!(map_event("UserPromptSubmit"), Some("PreInvocation"));
    // No agy analog -> skipped, never guessed. `SessionStart` once mapped to itself
    // and `UserPromptSubmit` to `BeforeAgent`; neither name exists in the binary.
    for unmapped in ["SessionStart", "SessionEnd", "SubagentStop", "PreCompact", "Notification"] {
        assert_eq!(map_event(unmapped), None, "{unmapped} should be skipped");
    }
}

#[test]
fn hook_portability_matches_the_mcp_rule() {
    assert!(hook_is_portable(&hook("Stop", "host_fixture self-heal")));
    assert!(!hook_is_portable(&hook("Stop", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh")));
}

#[test]
fn reconcile_hooks_writes_the_plugin_keyed_tree_then_noops() {
    let path = scratch("hooks.json");
    let hooks = [hook("UserPromptSubmit", "host_fixture check-restart"), hook("Stop", "host_fixture bye")];

    assert!(reconcile_hooks(&path, "ez-fixture-plugin", &hooks, true).unwrap());
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    // Exact shape for a non-tool event: our top-level plugin key, then a FLAT
    // `{type, command}` handler list per event.
    assert_eq!(
        v["ez-fixture-plugin"]["PreInvocation"],
        json!([{ "type": "command", "command": "host_fixture check-restart" }]),
        "UserPromptSubmit was not mapped to a flat PreInvocation"
    );
    assert_eq!(v["ez-fixture-plugin"]["Stop"], json!([{ "type": "command", "command": "host_fixture bye" }]));

    // idempotent: a rebuilt-identical subtree is a true NoOp (no write).
    assert!(!reconcile_hooks(&path, "ez-fixture-plugin", &hooks, true).unwrap(), "second reconcile should no-op");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn tool_events_get_the_grouped_matcher_wrapper() {
    let path = scratch("hooks.json");
    // `agy` reads PreToolUse/PostToolUse as `{matcher, hooks:[handler,…]}`; a flat
    // handler with a sibling matcher puts the command at a level nothing reads.
    let matched = HookBinding { event: "PreToolUse".into(), matcher: Some("run_command".into()), command: "host_fixture guard".into() };

    assert!(reconcile_hooks(&path, "ez-fixture-plugin", std::slice::from_ref(&matched), true).unwrap());
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        v["ez-fixture-plugin"]["PreToolUse"],
        json!([{ "matcher": "run_command", "hooks": [{ "type": "command", "command": "host_fixture guard" }] }])
    );

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn a_matcherless_tool_hook_groups_under_the_wildcard() {
    let path = scratch("hooks.json");
    // CC treats an absent matcher as "every tool". The grouped shape has nowhere to
    // put that but the matcher itself, so it becomes agy's own `*` (the value its
    // doc's PostToolUse example uses) rather than an omitted key of unknown meaning.
    let hooks = [hook("PostToolUse", "host_fixture audit")];

    assert!(reconcile_hooks(&path, "ez-fixture-plugin", &hooks, true).unwrap());
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        v["ez-fixture-plugin"]["PostToolUse"],
        json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": "host_fixture audit" }] }])
    );

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn tool_hooks_sharing_a_matcher_fold_into_one_group() {
    let path = scratch("hooks.json");
    // One group per matcher, handlers stacked inside it: the shape CC itself uses and
    // the one agy's doc shows. Two groups with the same matcher would be ambiguous.
    let mk =
        |command: &str, matcher: &str| HookBinding { event: "PreToolUse".into(), matcher: Some(matcher.into()), command: command.into() };
    let hooks = [mk("first", "run_command"), mk("second", "run_command"), mk("other", "edit_file")];

    assert!(reconcile_hooks(&path, "ez-fixture-plugin", &hooks, true).unwrap());
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        v["ez-fixture-plugin"]["PreToolUse"],
        json!([
            { "matcher": "edit_file", "hooks": [{ "type": "command", "command": "other" }] },
            { "matcher": "run_command", "hooks": [{ "type": "command", "command": "first" }, { "type": "command", "command": "second" }] },
        ])
    );

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn reconcile_hooks_skips_non_portable_and_writes_no_file() {
    let path = scratch("hooks.json");
    let rooted = hook("Stop", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh");

    assert!(!reconcile_hooks(&path, "ez-fixture-plugin", std::slice::from_ref(&rooted), true).unwrap());
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

// --- enabled:false carry-through (§6) -----------------------------------------

#[test]
fn reconcile_hooks_preserves_a_disable_on_self_heal_but_an_explicit_install_reenables() {
    let path = scratch("hooks.json");
    let hooks = [hook("Stop", "host_fixture bye")];

    // Explicit install: writes the tree with no `enabled` key (default-enabled).
    assert!(reconcile_hooks(&path, "ez-fixture-plugin", &hooks, true).unwrap());
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(v["ez-fixture-plugin"].get("enabled").is_none(), "a fresh install must not write an enabled key");

    // A user disables our plugin's hooks by hand, and the event tree separately
    // drifts (a hand edit), so the repair below has something real to fix.
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    doc["ez-fixture-plugin"]["enabled"] = json!(false);
    doc["ez-fixture-plugin"]["Stop"] = json!([{ "type": "command", "command": "stale-command" }]);
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();

    // self_heal/adopt (reenable=false) repairs the drifted event but must not flip
    // the disable back on.
    assert!(reconcile_hooks(&path, "ez-fixture-plugin", &hooks, false).unwrap(), "the drifted event must still be repaired");
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["ez-fixture-plugin"]["enabled"], json!(false), "self_heal must preserve a user's deliberate disable");
    assert_eq!(
        v["ez-fixture-plugin"]["Stop"],
        json!([{ "type": "command", "command": "host_fixture bye" }]),
        "drifted event was not repaired"
    );

    // An explicit install/update (reenable=true) honors the user's request and
    // re-enables by omitting the key again.
    assert!(reconcile_hooks(&path, "ez-fixture-plugin", &hooks, true).unwrap());
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(v["ez-fixture-plugin"].get("enabled").is_none(), "an explicit install must re-enable (omit the key)");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn probe_hooks_reports_disabled_when_enabled_is_false_even_with_drift() {
    let path = scratch("hooks.json");
    let hooks = [hook("Stop", "host_fixture bye")];
    let tree = || render_hook_tree(&hooks);

    // Absent file -> no surface owned yet.
    assert!(matches!(probe_hooks(&path, "ez-fixture-plugin", tree()).unwrap(), Some(BackendState::Absent)));

    // Healthy, matching write.
    reconcile_hooks(&path, "ez-fixture-plugin", &hooks, true).unwrap();
    assert!(matches!(probe_hooks(&path, "ez-fixture-plugin", tree()).unwrap(), Some(BackendState::Healthy)));

    // Disabled + otherwise drifted (a stale event list): still reads Disabled, not
    // NeedsRepair — the deliberate disable freezes the backend regardless of drift.
    std::fs::write(&path, r#"{"ez-fixture-plugin":{"enabled":false,"Stop":[{"type":"command","command":"stale-command"}]}}"#).unwrap();
    assert!(matches!(probe_hooks(&path, "ez-fixture-plugin", tree()).unwrap(), Some(BackendState::Disabled)));

    // No subtree owned for this plugin (`tree()` is `None`) never reports Disabled
    // even if the file happens to carry someone else's `enabled:false`.
    assert!(probe_hooks(&path, "ez-fixture-plugin", None).unwrap().is_none());

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
