//! `report::compose` unit tests: the total precedence the per-surface probe
//! composition rests on. Every row of the precedence table (incl. the mixed cases)
//! is pinned here so a rule reorder reds immediately.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::compose;
use crate::agents::BackendState::{self, Absent, Disabled, Healthy, NeedsRepair};

/// `compose` returns one of four states; name them for exact assertions without
/// needing `BackendState: PartialEq` in production.
fn tag(state: BackendState) -> &'static str {
    match state {
        Absent => "absent",
        Healthy => "healthy",
        Disabled => "disabled",
        NeedsRepair => "needs_repair",
    }
}

fn composed(states: impl IntoIterator<Item = BackendState>) -> &'static str {
    tag(compose(states))
}

#[test]
fn empty_is_healthy_so_a_surfaceless_backend_keeps_its_marker() {
    // No surface contributed (the mcp-less/hook-less plugin) must never read Absent —
    // that would drop a present marker in self_heal.
    assert_eq!(composed([]), "healthy");
}

#[test]
fn single_surface_is_identity() {
    // compose([Some(mcp)]) == mcp keeps every mcp-only path byte-for-byte unchanged.
    assert_eq!(composed([Healthy]), "healthy");
    assert_eq!(composed([Absent]), "absent");
    assert_eq!(composed([Disabled]), "disabled");
    assert_eq!(composed([NeedsRepair]), "needs_repair");
}

#[test]
fn any_disabled_freezes_the_backend() {
    // A deliberate disable outranks everything else, so self_heal no-ops (never re-enables).
    assert_eq!(composed([Disabled, Healthy]), "disabled");
    assert_eq!(composed([Healthy, Disabled]), "disabled");
    assert_eq!(composed([Disabled, NeedsRepair]), "disabled");
    assert_eq!(composed([Disabled, Absent]), "disabled");
}

#[test]
fn all_absent_is_absent_so_a_gone_plugin_is_never_resurrected() {
    assert_eq!(composed([Absent, Absent]), "absent");
    assert_eq!(composed([Absent, Absent, Absent]), "absent");
}

#[test]
fn partial_absent_is_needs_repair() {
    // Some surfaces gone, some present -> a partial deletion the reconcile re-adds,
    // NOT a whole-backend Absent that would orphan the surviving surface.
    assert_eq!(composed([Healthy, Absent]), "needs_repair");
    assert_eq!(composed([Absent, Healthy]), "needs_repair");
    assert_eq!(composed([Healthy, Healthy, Absent]), "needs_repair");
}

#[test]
fn any_needs_repair_wins_over_healthy() {
    // The headline bug: a broken hook surface behind a healthy mcp surface must
    // surface as NeedsRepair, not Healthy.
    assert_eq!(composed([NeedsRepair, Healthy]), "needs_repair");
    assert_eq!(composed([Healthy, NeedsRepair]), "needs_repair");
}

#[test]
fn all_healthy_is_healthy() {
    assert_eq!(composed([Healthy, Healthy, Healthy]), "healthy");
}
