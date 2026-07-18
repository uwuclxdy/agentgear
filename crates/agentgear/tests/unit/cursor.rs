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

    let dir = std::env::temp_dir().join(format!("ez-cursor-subagentstart-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("hooks.json");
    let sub = HookBinding { event: "SubagentStart".into(), matcher: None, command: "host_fixture note".into() };

    assert!(reconcile_hooks(&path, std::slice::from_ref(&sub)).unwrap(), "SubagentStart hook must be written");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["hooks"]["subagentStart"][0]["command"], "host_fixture note", "SubagentStart must land under hooks.subagentStart:\n{v}");

    std::fs::remove_dir_all(&dir).ok();
}
