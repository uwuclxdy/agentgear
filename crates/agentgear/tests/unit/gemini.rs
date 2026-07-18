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
