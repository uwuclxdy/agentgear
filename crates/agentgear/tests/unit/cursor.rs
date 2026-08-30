//! cursor backend unit tests: the hook event map. cursor renames CC events to its
//! own camelCase lifecycle names (`SessionStart` -> `sessionStart`, ...). This pins
//! that `SubagentStart` maps to cursor's real `subagentStart` agent hook (present in
//! the binary-confirmed 21-event catalog, verify-cursor #4), mirroring `SubagentStop`.

use super::*;
use crate::components::HookBinding;

/// cursor hosts a `subagentStart` agent hook (verify-cursor #4) and already maps
/// `SubagentStop -> subagentStop`, so the CC `SubagentStart` event must map to
/// `subagentStart` too. Guards the `map_event` arm end-to-end through `reconcile_hooks`:
/// without it a CC plugin's `SubagentStart` hook is dropped even though cursor runs it.
#[test]
fn subagent_start_maps_to_cursors_camelcase_event() {
    assert_eq!(map_event("SubagentStart"), Some("subagentStart"), "SubagentStart must map to cursor's subagentStart");

    let dir = crate::scratch::path("ez-cursor-subagentstart");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("hooks.json");
    let sub = HookBinding { event: "SubagentStart".into(), matcher: None, command: "host_fixture note".into() };

    assert!(reconcile_hooks(&path, std::slice::from_ref(&sub)).unwrap(), "SubagentStart hook must be written");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["hooks"]["subagentStart"][0]["command"], "host_fixture note", "SubagentStart must land under hooks.subagentStart:\n{v}");

    std::fs::remove_dir_all(&dir).ok();
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

    let backend = super::CursorBackend;
    let id = backend.id();

    let src = crate::scratch::path("ez-cidtok-src");
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

    let plugin = Plugin { name: "ez-cid", marketplace: "ez-mkt", version: "0.1.0", agents: &["cursor"], instructions: None, blob: &[] };
    let project = crate::scratch::path("ez-cidtok-dst");
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
