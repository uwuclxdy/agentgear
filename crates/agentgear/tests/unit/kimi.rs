//! kimi backend unit tests: the bespoke `[[hooks]]` toml path (exact shape,
//! idempotency, exact removal under a colliding event) plus the portability filter
//! for both mcp servers and hooks — a `${CLAUDE_PLUGIN_ROOT}`-bearing entry is never
//! written and so must never be a removal candidate either. The mcp side rides the
//! shared `mcpjson` renderer (covered by its own tests + the hermetic e2e), so these
//! focus on what kimi owns.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{hook_is_portable, hook_present, map_event, reconcile_hooks, remove_hooks};
use crate::agents::mcpjson::{self, ServerShape};
use crate::components::{HookBinding, McpKind, McpServer};
use crate::host::Outcome;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-kimi-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: Vec::new(), env: BTreeMap::new() }
}

fn hook(event: &str, command: &str) -> HookBinding {
    HookBinding { event: event.into(), matcher: None, command: command.into() }
}

#[test]
fn hook_portability_matches_mcp_server_rule() {
    let portable = hook("SessionStart", "host_fixture self-heal");
    let rooted = hook("SessionStart", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh");
    assert!(hook_is_portable(&portable));
    assert!(!hook_is_portable(&rooted));
}

#[test]
fn map_event_passes_kimi_events_and_skips_the_unknown() {
    // Kimi mirrors CC event names 1:1 for the overlap set.
    assert_eq!(map_event("SessionStart"), Some("SessionStart"));
    assert_eq!(map_event("UserPromptSubmit"), Some("UserPromptSubmit"));
    assert_eq!(map_event("PreToolUse"), Some("PreToolUse"));
    // A name kimi does not define is skipped rather than guessed.
    assert_eq!(map_event("PreToolFictional"), None);
}

#[test]
fn reconcile_hooks_writes_expected_toml_shape_and_second_reconcile_noops() {
    let path = scratch("config.toml");
    let matched = HookBinding { event: "PreToolUse".into(), matcher: Some("Bash".into()), command: "host_fixture guard".into() };
    let hooks = [hook("SessionStart", "host_fixture self-heal"), matched];

    let changed = reconcile_hooks(&path, &hooks).unwrap();
    assert!(changed, "first reconcile must write");
    let text = std::fs::read_to_string(&path).unwrap();

    // Exact `[[hooks]]` array-of-tables shape, event names 1:1, matcher only when set.
    assert!(text.contains("[[hooks]]"), "no array-of-tables emitted:\n{text}");
    assert!(text.contains("event = \"SessionStart\""), "SessionStart event missing:\n{text}");
    assert!(text.contains("command = \"host_fixture self-heal\""), "SessionStart command missing:\n{text}");
    assert!(text.contains("event = \"PreToolUse\""), "PreToolUse event missing:\n{text}");
    assert!(text.contains("matcher = \"Bash\""), "matcher missing for the matched hook:\n{text}");
    assert!(text.contains("command = \"host_fixture guard\""), "PreToolUse command missing:\n{text}");
    // A hook with no matcher must not emit a matcher key.
    assert!(!text.contains("matcher = \"\""), "an empty matcher key leaked in:\n{text}");
    // Round-trips through a real TOML parser (proves valid `[[hooks]]`, not just shaped text).
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(doc["hooks"].as_array_of_tables().unwrap().len(), 2, "expected two hook tables");

    // Idempotent: a converged reconcile touches no bytes.
    let changed = reconcile_hooks(&path, &hooks).unwrap();
    assert!(!changed, "second reconcile must be a NoOp");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "second reconcile changed bytes");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn reconcile_hooks_skips_non_portable_and_remove_hooks_leaves_a_same_command_survivor() {
    let path = scratch("config.toml");
    let rooted = hook("SessionStart", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh");

    // A non-portable hook is never written (and no empty file is created).
    let changed = reconcile_hooks(&path, std::slice::from_ref(&rooted)).unwrap();
    assert!(!changed, "a non-portable hook must not be written");
    assert!(!path.exists(), "reconcile_hooks must not create a file for zero writable hooks");

    // Seed a user hook whose command equals the literal (unexpanded) non-portable
    // command we would have written — proves `remove_hooks` never touches it.
    let seed = format!("[[hooks]]\nevent = \"SessionStart\"\ncommand = \"{}\"\n", rooted.command);
    std::fs::write(&path, &seed).unwrap();
    let removed = remove_hooks(&path, std::slice::from_ref(&rooted)).unwrap();
    assert!(!removed, "remove_hooks must not remove a hook it never wrote");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), seed, "seeded hook survived byte-for-byte");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn remove_hooks_strips_ours_keeps_user_under_a_colliding_event() {
    let path = scratch("config.toml");
    // A user hook under SessionStart (the same event we write to) with its own command.
    let seed = "[[hooks]]\nevent = \"SessionStart\"\ncommand = \"their-session-hook\"\n";
    std::fs::write(&path, seed).unwrap();

    let ours = hook("SessionStart", "host_fixture self-heal");
    assert!(reconcile_hooks(&path, std::slice::from_ref(&ours)).unwrap(), "our hook should be appended");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("their-session-hook") && text.contains("host_fixture self-heal"), "both hooks should coexist:\n{text}");

    // Remove ours: the user's same-event hook must survive; the file must still parse.
    assert!(remove_hooks(&path, std::slice::from_ref(&ours)).unwrap(), "our hook should be removed");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("host_fixture self-heal"), "our hook survived removal:\n{text}");
    assert!(text.contains("their-session-hook"), "user's same-event hook was removed:\n{text}");
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(doc["hooks"].as_array_of_tables().unwrap().len(), 1, "only the user hook should remain");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn hook_present_matches_on_event_matcher_and_command() {
    let path = scratch("config.toml");
    let matched = HookBinding { event: "PreToolUse".into(), matcher: Some("Bash".into()), command: "host_fixture guard".into() };
    reconcile_hooks(&path, std::slice::from_ref(&matched)).unwrap();
    let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    let arr = doc["hooks"].as_array_of_tables().unwrap();

    assert!(hook_present(arr, "PreToolUse", &matched), "identical entry must count as present");
    // A different matcher is a distinct entry (not the same hook).
    let other_matcher = HookBinding { matcher: Some("Edit".into()), ..matched.clone() };
    assert!(!hook_present(arr, "PreToolUse", &other_matcher), "a differing matcher must not match");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mcp_probe_classifies_absent_healthy_and_needs_repair() {
    // The kimi backend routes mcp through the shared json renderer, so drive it the
    // same way `reconcile`/`probe` do (mcp.json `mcpServers`, Plain shape).
    let path = scratch("mcp.json");
    let servers = [server("ez-fixture", "host_fixture")];

    // Absent: nothing written yet.
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), crate::agents::BackendState::Absent));

    // Healthy: after a reconcile the on-disk body matches.
    assert_eq!(mcpjson::reconcile(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), Outcome::Installed);
    assert!(matches!(
        mcpjson::probe(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(),
        crate::agents::BackendState::Healthy
    ));

    // NeedsRepair: the key is present but its body drifted from what we'd render.
    std::fs::write(&path, r#"{"mcpServers":{"ez-fixture":{"command":"tampered","args":[],"env":{}}}}"#).unwrap();
    assert!(matches!(
        mcpjson::probe(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(),
        crate::agents::BackendState::NeedsRepair
    ));

    // A plugin with only a non-portable server is Healthy (never Absent), so a
    // present marker is never dropped for an mcp-less-after-filtering plugin.
    let rooted = [server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky")];
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &rooted, ServerShape::plain()).unwrap(), crate::agents::BackendState::Healthy));

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

    let backend = super::KimiBackend;
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

    let plugin = Plugin {
        name: "ez-cid",
        marketplace: "ez-mkt",
        version: "0.1.0",
        agents: &["kimi"],
        instructions: None,
        statusline: None,
        blob: &[],
    };
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
