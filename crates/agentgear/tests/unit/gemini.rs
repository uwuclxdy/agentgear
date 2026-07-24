//! gemini backend unit tests: portability filtering for both mcp servers and
//! hooks, since a `${CLAUDE_PLUGIN_ROOT}`-bearing entry is never written and
//! must therefore never be a candidate for removal either (removing an
//! unfiltered name could delete an unrelated user-owned entry of the same name).

use super::{hook_is_portable, reconcile_hooks, remove_hooks};
use crate::components::HookBinding;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-gemini-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn subagent_events_are_unmapped() {
    // gemini's 11-event HookEventName enum (verify-gemini #19) has no subagent
    // event; the lone agent-lifecycle analog AfterAgent would over-fire on the
    // main agent, so both refuse rather than write a hook that never fires right.
    assert_eq!(super::map_event("SubagentStart"), None);
    assert_eq!(super::map_event("SubagentStop"), None);
    // control: a mapped event still resolves, so the refusal above is not vacuous.
    assert_eq!(super::map_event("SessionStart"), Some("SessionStart"));
}

#[test]
fn hook_portability_matches_mcp_server_rule() {
    let portable = HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() };
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh".into() };
    assert!(hook_is_portable(&portable));
    assert!(!hook_is_portable(&rooted));
}

#[test]
fn reconcile_hooks_skips_non_portable_and_remove_hooks_leaves_a_same_name_survivor() {
    let path = scratch("settings.json");
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh".into() };

    // A non-portable hook is never written.
    let changed = reconcile_hooks(&path, std::slice::from_ref(&rooted)).unwrap();
    assert!(!changed, "a non-portable hook must not be written");
    assert!(!path.exists(), "reconcile_hooks must not create a file for zero writable hooks");

    // Seed the file with a user's own hook whose command happens to equal the
    // literal (unexpanded) non-portable command we would have written — proves
    // `remove_hooks` never touches it, since we never wrote it.
    std::fs::write(&path, format!(r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}"}}]}}]}}}}"#, rooted.command))
        .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    let removed = remove_hooks(&path, std::slice::from_ref(&rooted)).unwrap();
    assert!(!removed, "remove_hooks must not remove a hook it never wrote");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "seeded hook survived byte-for-byte");

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

    let backend = super::GeminiBackend;
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

    let plugin = Plugin { name: "ez-cid", marketplace: "ez-mkt", version: "0.1.0", agents: &["gemini"], instructions: None, blob: &[] };
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
