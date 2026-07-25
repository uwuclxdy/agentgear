//! The slot renderer's pure decisions, shared by every status-line backend: what a
//! declaration renders to (a literal-key pin, so a field rename cannot silently
//! change what lands in a harness's settings), and whose value is already in the
//! slot. The disk half (stash, write, restore) needs a real data root, so it is
//! covered by the host-fixture hermetic tests.

use serde_json::{Value, json};

use super::{SlotShape, is_ours, rendered, state};
use crate::agents::BackendState;
use crate::host::Plugin;
use crate::statusline::StatusLineDecl;

const SHAPE: SlotShape = SlotShape::typed_command();

fn plugin_with(statusline: Option<StatusLineDecl>) -> Plugin {
    Plugin { name: "ez-sl", marketplace: "ez-mkt", version: "0.1.0", agents: &["claude"], instructions: None, statusline, blob: &[] }
}

#[test]
fn rendered_pins_the_command_object_shape() {
    // Literal keys, pinned: the slot value is a published on-disk contract, so a
    // field rename must not be able to move them.
    let plugin = plugin_with(Some(StatusLineDecl::new("mytool statusline")));
    let (value, command) = rendered(&plugin, "claude", SHAPE).expect("a declared status line must render");
    assert_eq!(command, "mytool statusline");
    assert_eq!(value, json!({"type": "command", "command": "mytool statusline"}));
}

#[test]
fn rendered_includes_padding_only_when_declared() {
    let plugin = plugin_with(Some(StatusLineDecl::new("mytool statusline").with_padding(0)));
    let (with, _) = rendered(&plugin, "claude", SHAPE).expect("a declared status line must render");
    assert_eq!(with, json!({"type": "command", "command": "mytool statusline", "padding": 0}));

    let (without, _) = rendered(&plugin_with(Some(StatusLineDecl::new("mytool statusline"))), "claude", SHAPE)
        .expect("a declared status line must render");
    assert!(without.get("padding").is_none(), "an unset padding must not emit the key: {without}");
}

#[test]
fn rendered_expands_the_client_token_per_backend() {
    // One declaration serves every harness that has a slot: each backend bakes its
    // own id, which is what lets the host read the right marker's stash back.
    let plugin = plugin_with(Some(StatusLineDecl::new("mytool statusline --client ${AGENTGEAR_CLIENT}").with_padding(0)));
    let (value, command) = rendered(&plugin, "qwen-code", SHAPE).expect("a declared status line must render");
    assert_eq!(command, "mytool statusline --client qwen-code");
    assert_eq!(value, json!({"type": "command", "command": "mytool statusline --client qwen-code", "padding": 0}));

    let (_, claude) = rendered(&plugin, "claude", SHAPE).expect("a declared status line must render");
    assert_eq!(claude, "mytool statusline --client claude");
}

#[test]
fn rendered_is_none_without_a_declaration() {
    assert!(rendered(&plugin_with(None), "claude", SHAPE).is_none());
}

#[test]
fn rendered_is_none_for_a_blank_command() {
    // `StatusLineDecl::default()` carries one. Rendering it would displace (and
    // stash) the user's real status line in exchange for a command that does
    // nothing, so a host bug here must cost them nothing.
    assert!(rendered(&plugin_with(Some(StatusLineDecl::default())), "claude", SHAPE).is_none());
    assert!(rendered(&plugin_with(Some(StatusLineDecl::new("   ").with_padding(0))), "claude", SHAPE).is_none());
}

#[test]
fn is_ours_matches_on_the_command_not_the_whole_object() {
    let ours = "mytool statusline --client claude";
    // Our own earlier rendering, padding since changed by a host release: still ours,
    // so it is never stashed as the user's original.
    assert!(is_ours(&json!({"type": "command", "command": ours, "padding": 1}), ours, SHAPE));
    assert!(is_ours(&json!({"type": "command", "command": ours}), ours, SHAPE));
    // Genuinely someone else's, and shapes with no command at all.
    assert!(!is_ours(&json!({"type": "command", "command": "their-bar", "padding": 0}), ours, SHAPE));
    assert!(!is_ours(&json!({"type": "command"}), ours, SHAPE));
    assert!(!is_ours(&json!("their-bar"), ours, SHAPE));
}

// `state`'s carried comparison. This is the layer the drift loop is visible at: a
// carried field missing from the convergence test makes `state` return `NeedsRepair`
// forever, and self_heal reconciles on every pass — but the reconcile writes the same
// bytes, so the OUTCOME stays `NoOp` and no lifecycle test can see it.

const CARRY_SHAPE: SlotShape = SlotShape::typed_command().carrying(&["enabled"], "re-enable it.");
const SLOT: &[&str] = &["statusLine"];

/// Write `slot` into a scratch settings file and classify it.
fn state_of(slot: Value, shape: SlotShape) -> BackendState {
    let dir = std::env::temp_dir().join(format!("ez-slotstate-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("settings.json");
    std::fs::write(&path, serde_json::to_vec(&json!({ "statusLine": slot })).expect("json")).expect("write");
    let plugin = plugin_with(Some(StatusLineDecl::new("mytool statusline").with_padding(0)));
    let verdict = state(&path, SLOT, &plugin, "claude", shape).expect("state must classify");
    let _ = std::fs::remove_dir_all(&dir);
    verdict.expect("a declared status line always contributes a state")
}

#[test]
fn a_carried_field_reads_as_converged_not_as_drift() {
    // The user's `enabled` rides along in what we write, so the live slot legitimately
    // carries a key our bare rendering does not. Compare against the bare rendering and
    // this is `NeedsRepair` on every pass, forever.
    let live = json!({"type": "command", "command": "mytool statusline", "padding": 0, "enabled": false});
    assert!(matches!(state_of(live, CARRY_SHAPE), BackendState::Healthy), "a carried field must not read as drift");
}

#[test]
fn drift_in_a_field_we_render_is_still_drift() {
    // The carry must not swallow real drift: `padding` is ours to render, so a changed
    // one is a repair even while `enabled` is carried.
    let live = json!({"type": "command", "command": "mytool statusline", "padding": 9, "enabled": false});
    assert!(matches!(state_of(live, CARRY_SHAPE), BackendState::NeedsRepair), "rendered-field drift must survive the carry");
}

#[test]
fn an_uncarried_shape_reads_an_extra_key_as_drift() {
    // The claude/qwen/droid shapes carry nothing, so an unexpected key IS drift there.
    // Pins that `carrying` is opt-in rather than a blanket loosening of convergence.
    let live = json!({"type": "command", "command": "mytool statusline", "padding": 0, "enabled": false});
    assert!(matches!(state_of(live, SHAPE), BackendState::NeedsRepair), "an uncarried shape must still see an extra key as drift");
}
