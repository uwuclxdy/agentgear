//! augment backend unit tests: the combined settings.json edit (one file holds
//! mcpServers + hooks in CC's nested shape), event mapping (`UserPromptSubmit`
//! renames to `PromptSubmit`), exact merge-safe removal, and the portability filter for
//! both mcp servers and hooks — a `${CLAUDE_PLUGIN_ROOT}`-bearing entry is never
//! written and so must never be a removal candidate either. The mcp probe rides the
//! shared `mcpjson` renderer (Plain shape), so it is exercised the same way the
//! backend's `probe` drives it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use super::{hook_is_portable, map_event, portable_names, reconcile_settings, remove_from_settings};
use crate::agents::BackendState;
use crate::agents::mcpjson::{self, ServerShape};
use crate::components::{HookBinding, McpKind, McpServer};
use crate::host::Outcome;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-augment-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: Vec::new(), env: BTreeMap::new() }
}

fn hook(event: &str, command: &str) -> HookBinding {
    HookBinding { event: event.into(), matcher: None, command: command.into() }
}

fn read(path: &PathBuf) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn portable_names_excludes_claude_plugin_root_servers() {
    let servers = [server("ez-fixture", "host_fixture"), server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky")];
    assert_eq!(portable_names(&servers), vec!["ez-fixture"]);
}

#[test]
fn hook_portability_matches_mcp_server_rule() {
    let portable = hook("SessionStart", "host_fixture self-heal");
    let rooted = hook("SessionStart", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh");
    assert!(hook_is_portable(&portable));
    assert!(!hook_is_portable(&rooted));
}

#[test]
fn map_event_covers_every_event_augment_hosts() {
    // Augment's validator is `.strict()` over exactly seven events, five of which
    // carry CC's own name.
    assert_eq!(map_event("SessionStart"), Some("SessionStart"));
    assert_eq!(map_event("SessionEnd"), Some("SessionEnd"));
    assert_eq!(map_event("PreToolUse"), Some("PreToolUse"));
    assert_eq!(map_event("PostToolUse"), Some("PostToolUse"));
    assert_eq!(map_event("Stop"), Some("Stop"));
    assert_eq!(map_event("Notification"), Some("Notification"));
    // The sixth is a straight rename: augment receives the prompt text on it, and
    // CC's own name is rejected outright (0 occurrences in the bundle).
    assert_eq!(map_event("UserPromptSubmit"), Some("PromptSubmit"));
    // Genuinely absent from the seven: skipped, not guessed.
    assert_eq!(map_event("PreCompact"), None);
    assert_eq!(map_event("SubagentStop"), None);

    // Nothing outside the accepted set may ever be emitted: augment rejects an
    // unlisted key as "Invalid event type".
    const ACCEPTED: [&str; 7] = ["PreToolUse", "PostToolUse", "Stop", "SessionStart", "SessionEnd", "Notification", "PromptSubmit"];
    for cc in [
        "SessionStart",
        "SessionEnd",
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "Notification",
        "UserPromptSubmit",
        "PreCompact",
        "SubagentStop",
    ] {
        if let Some(mapped) = map_event(cc) {
            assert!(ACCEPTED.contains(&mapped), "{cc} maps to `{mapped}`, which augment's validator rejects");
        }
    }
}

#[test]
fn reconcile_settings_writes_mcp_and_hooks_in_one_file_and_second_reconcile_noops() {
    let path = scratch("settings.json");
    let servers = [server("ez-fixture", "host_fixture")];
    let matched = HookBinding { event: "PreToolUse".into(), matcher: Some("Bash".into()), command: "host_fixture guard".into() };
    // An identity-mapped hook, a matched hook, a renamed one, and one augment does
    // not host (which must be dropped).
    let hooks = [
        hook("SessionStart", "host_fixture self-heal"),
        matched,
        hook("UserPromptSubmit", "host_fixture check-restart"),
        hook("PreCompact", "host_fixture squeeze"),
    ];

    let changed = reconcile_settings(&path, &servers, &hooks).unwrap();
    assert!(changed, "first reconcile must write");
    let root = read(&path);

    // mcp: our server under `mcpServers`, Plain `{command,args,env}` shape.
    let entry = &root["mcpServers"]["ez-fixture"];
    assert_eq!(entry["command"], Value::from("host_fixture"), "mcp command wrong:\n{root:#}");
    assert!(entry.get("type").is_none(), "Plain shape must not emit a `type` key:\n{root:#}");

    // hooks: CC nested shape under mapped event names, matcher only when set.
    let session = root["hooks"]["SessionStart"].as_array().unwrap();
    assert_eq!(session[0]["hooks"][0]["command"], Value::from("host_fixture self-heal"), "SessionStart command missing:\n{root:#}");
    assert_eq!(session[0]["hooks"][0]["type"], Value::from("command"), "hook handler must be a command type:\n{root:#}");
    let pretool = root["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pretool[0]["matcher"], Value::from("Bash"), "matcher missing for the matched hook:\n{root:#}");

    // UserPromptSubmit lands under augment's own spelling, never CC's (which the
    // validator rejects outright, taking the whole hooks object with it).
    let prompt = root["hooks"]["PromptSubmit"].as_array().unwrap();
    assert_eq!(prompt[0]["hooks"][0]["command"], Value::from("host_fixture check-restart"), "PromptSubmit command missing:\n{root:#}");
    assert!(root["hooks"].get("UserPromptSubmit").is_none(), "CC's own event name must never be written:\n{root:#}");

    // PreCompact is genuinely absent from augment: neither key nor command lands.
    assert!(root["hooks"].get("PreCompact").is_none(), "PreCompact must not be written:\n{root:#}");
    assert!(!std::fs::read_to_string(&path).unwrap().contains("squeeze"), "the unmapped hook's command leaked in");

    // Idempotent: a converged reconcile touches no bytes.
    let bytes = std::fs::read(&path).unwrap();
    let changed = reconcile_settings(&path, &servers, &hooks).unwrap();
    assert!(!changed, "second reconcile must be a NoOp");
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "second reconcile changed bytes");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn reconcile_settings_skips_non_portable_and_remove_leaves_a_same_command_survivor() {
    let path = scratch("settings.json");
    let rooted_server = server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky");
    let rooted_hook = hook("SessionStart", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh");

    // Only non-portable entries -> nothing writable -> no file created.
    let changed = reconcile_settings(&path, std::slice::from_ref(&rooted_server), std::slice::from_ref(&rooted_hook)).unwrap();
    assert!(!changed, "non-portable entries must not be written");
    assert!(!path.exists(), "reconcile_settings must not create a file for zero writable entries");

    // Seed a user config whose entries share the literal (unexpanded) names we would
    // have written — proves remove never touches what we never wrote.
    let seed = format!(
        r#"{{"mcpServers":{{"rooted":{{"command":"user-owned"}}}},"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}"}}]}}]}}}}"#,
        rooted_hook.command
    );
    std::fs::write(&path, &seed).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    let removed =
        remove_from_settings(&path, &portable_names(std::slice::from_ref(&rooted_server)), std::slice::from_ref(&rooted_hook)).unwrap();
    assert!(!removed, "remove must not touch entries it never wrote");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "seeded entries survived byte-for-byte");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn remove_from_settings_strips_ours_keeps_user_entries() {
    let path = scratch("settings.json");
    // Pre-seed a foreign mcp server + a user hook under SessionStart (the event we write to).
    let seed = r#"{
      "theme": "dark",
      "mcpServers": { "theirs": { "command": "their-server", "args": [] } },
      "hooks": { "SessionStart": [{ "hooks": [{ "type": "command", "command": "their-session-hook" }] }] }
    }"#;
    std::fs::write(&path, seed).unwrap();

    let servers = [server("ez-fixture", "host_fixture")];
    let hooks = [hook("SessionStart", "host_fixture self-heal")];
    assert!(reconcile_settings(&path, &servers, &hooks).unwrap(), "our entries should be added");
    let root = read(&path);
    assert!(root["mcpServers"]["ez-fixture"].is_object() && root["mcpServers"]["theirs"].is_object(), "both servers coexist");
    assert_eq!(root["hooks"]["SessionStart"].as_array().unwrap().len(), 2, "both SessionStart hook groups coexist");

    // Remove ours: the user's server, hook, and unrelated top-level key all survive.
    assert!(remove_from_settings(&path, &portable_names(&servers), &hooks).unwrap(), "our entries should be removed");
    let root = read(&path);
    assert!(root["mcpServers"].get("ez-fixture").is_none(), "our server survived removal:\n{root:#}");
    assert!(root["mcpServers"]["theirs"].is_object(), "the user's server was removed:\n{root:#}");
    assert_eq!(root["theme"], Value::from("dark"), "an unrelated top-level key was clobbered:\n{root:#}");
    let session = root["hooks"]["SessionStart"].as_array().unwrap();
    assert_eq!(session.len(), 1, "exactly the user's hook group remains:\n{root:#}");
    assert_eq!(session[0]["hooks"][0]["command"], Value::from("their-session-hook"), "the user's hook was removed:\n{root:#}");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mcp_probe_classifies_absent_healthy_and_needs_repair() {
    // The backend routes mcp through the shared json renderer, so drive it the same
    // way `probe` does (settings.json `mcpServers`, Plain shape).
    let path = scratch("settings.json");
    let servers = [server("ez-fixture", "host_fixture")];

    // Absent: nothing written yet.
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), BackendState::Absent));

    // Healthy: after a reconcile the on-disk body matches what we'd render.
    assert!(reconcile_settings(&path, &servers, &[]).unwrap());
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), BackendState::Healthy));

    // NeedsRepair: the key is present but its body drifted.
    std::fs::write(&path, r#"{"mcpServers":{"ez-fixture":{"command":"tampered","args":[],"env":{}}}}"#).unwrap();
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), BackendState::NeedsRepair));

    // A plugin with only a non-portable server is Healthy (never Absent), so a present
    // marker is never dropped for an mcp-less-after-filtering plugin.
    let rooted = [server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky")];
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &rooted, ServerShape::plain()).unwrap(), BackendState::Healthy));

    // Sanity: the shared reconcile agrees this is an Installed (not NoOp) first write.
    let fresh = scratch("settings.json");
    assert_eq!(mcpjson::reconcile(&fresh, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), Outcome::Installed);
    std::fs::remove_dir_all(fresh.parent().unwrap()).ok();

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

// --- ${AGENTGEAR_CLIENT} portability token -----------------------------------

/// Recursively true if any file under `root` has contents containing `needle`.
fn agentgear_token_dir_contains(root: &std::path::Path, needle: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(root) else { return false };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if agentgear_token_dir_contains(&p, needle) {
                return true;
            }
        } else if std::fs::read_to_string(&p).is_ok_and(|s| s.contains(needle)) {
            return true;
        }
    }
    false
}

/// A hook command AND an mcp arg both carry `${AGENTGEAR_CLIENT}`; after reconcile
/// each must read this backend's own id, reconcile↔probe must stay Healthy (no
/// perpetual NeedsRepair from a probe that forgot to substitute), and remove must
/// strip the substituted entry it wrote.
#[test]
fn agentgear_client_token_expands_to_this_backend_id() {
    use crate::agents::{AgentBackend, BackendState};
    use crate::host::{Desired, Plugin, Scope, Source};

    let backend = super::AugmentBackend;
    let id = backend.id();

    let src = std::env::temp_dir().join(format!("ez-cidtok-src-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(src.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(src.join("hooks")).unwrap();
    std::fs::write(
        src.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"ez-cid","version":"0.1.0","mcpServers":{"srv":{"command":"host_fixture","args":["cid=${AGENTGEAR_CLIENT}"]}}}"#,
    )
    .unwrap();
    std::fs::write(
        src.join("hooks").join("hooks.json"),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"host_fixture up --client ${AGENTGEAR_CLIENT}"}]}]}}"#,
    )
    .unwrap();

    let plugin = Plugin { name: "ez-cid", marketplace: "ez-mkt", version: "0.1.0", agents: &["augment"], instructions: None, blob: &[] };
    let project = std::env::temp_dir().join(format!("ez-cidtok-dst-{:016x}", fastrand::u64(..)));
    let scope = Scope::Project { path: project.clone() };
    let source = Source::Path(src.clone());

    backend.reconcile(&plugin, &Desired { source: source.clone(), reenable: true }, &scope).unwrap();

    assert!(agentgear_token_dir_contains(&project, &format!("cid={id}")), "the mcp arg token did not expand to `{id}`");
    assert!(!agentgear_token_dir_contains(&project, "${AGENTGEAR_CLIENT}"), "a raw ${{AGENTGEAR_CLIENT}} token leaked to disk");

    assert!(
        matches!(backend.probe(&plugin, &scope, &source).unwrap(), BackendState::Healthy),
        "a token-bearing install must probe Healthy, not churn"
    );

    backend.remove(&plugin, &scope, &source).unwrap();
    assert!(!agentgear_token_dir_contains(&project, &format!("cid={id}")), "remove left the substituted mcp entry behind");

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&project);
}
