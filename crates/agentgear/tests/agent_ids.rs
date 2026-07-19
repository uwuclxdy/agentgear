//! `AGENT_IDS` is the compiled-in id roster. Every entry must resolve through the
//! public `backend_for`, and a feature-off id must not appear: the const is
//! cfg-gated to mirror the registry, so a hand-edited copy that drifts from
//! `backend_for`'s arms is caught here rather than shipping a dead id.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agentgear::{AGENT_IDS, backend_for};

#[test]
fn every_id_resolves_through_backend_for() {
    assert!(!AGENT_IDS.is_empty(), "`claude` is always compiled in, so the roster is never empty");
    for &id in AGENT_IDS {
        assert!(backend_for(id).is_some(), "AGENT_IDS lists `{id}` but backend_for cannot resolve it");
    }
}

#[test]
fn claude_is_always_present() {
    assert!(AGENT_IDS.contains(&"claude"), "the default `claude` backend is always compiled in");
}

// Default build: `codex` is off, so it must be absent from both the roster and the
// registry. `--all-features` compiles this out, where the codex arm is live.
#[cfg(not(feature = "codex"))]
#[test]
fn a_feature_off_id_is_absent() {
    assert!(!AGENT_IDS.contains(&"codex"), "codex feature is off, so AGENT_IDS must not list it");
    assert!(backend_for("codex").is_none(), "codex feature is off, so backend_for must not resolve it");
}
