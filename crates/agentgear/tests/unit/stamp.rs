//! stamp unit tests: pure `Marker` (de)serialization and `source_from_marker`
//! (the resolution logic, split out precisely so it needs no I/O to test).
//! `read`/`write`/`clear`/`resolve_source` route through `data_root`
//! (`dirs::data_dir`), which can't be redirected without mutating process env —
//! forbidden in edition 2024 — so the disk-touching half is covered by the
//! host-fixture e2e (matching how `stamp.rs` is tested today).

use std::path::PathBuf;

use crate::host::Source;
use crate::stamp::{Marker, source_from_marker};

fn marker(source_mode: &str, source_path: Option<&str>) -> Marker {
    Marker {
        binary_version: "0.1.0".into(),
        plugin_version: "0.1.0".into(),
        source_mode: source_mode.into(),
        scope: "user".into(),
        agent: "claude".into(),
        project_path: None,
        source_path: source_path.map(String::from),
        statusline_original: None,
        statusline_command: None,
    }
}

#[test]
fn marker_without_source_path_still_deserializes() {
    // A marker written by a prior binary, before this field existed, carries no
    // `source_path` key at all.
    let json = r#"{
        "binary_version": "0.1.0",
        "plugin_version": "0.1.0",
        "source_mode": "embedded",
        "scope": "user",
        "agent": "claude"
    }"#;
    let marker: Marker = serde_json::from_str(json).expect("an old marker without source_path must still load");
    assert_eq!(marker.source_path, None);
}

#[test]
fn marker_with_source_path_round_trips() {
    let json = r#"{
        "binary_version": "0.1.0",
        "plugin_version": "0.1.0",
        "source_mode": "path",
        "scope": "user",
        "agent": "claude",
        "source_path": "/tmp/some/plugin"
    }"#;
    let marker: Marker = serde_json::from_str(json).expect("marker must parse");
    assert_eq!(marker.source_path.as_deref(), Some("/tmp/some/plugin"));

    let bytes = serde_json::to_vec(&marker).expect("marker must serialize");
    let round_tripped: Marker = serde_json::from_slice(&bytes).expect("round-tripped marker must parse");
    assert_eq!(round_tripped.source_path.as_deref(), Some("/tmp/some/plugin"));
}

#[test]
fn marker_without_statusline_original_still_deserializes() {
    // Every marker written before the statusLine surface existed carries no such
    // key; it must load as "nothing was stashed", not fail the whole marker read
    // (a failed read is treated as absent, which would silently re-adopt installs).
    let json = r#"{
        "binary_version": "0.1.0",
        "plugin_version": "0.1.0",
        "source_mode": "embedded",
        "scope": "user",
        "agent": "claude"
    }"#;
    let marker: Marker = serde_json::from_str(json).expect("an old marker without statusline_original must still load");
    assert_eq!(marker.statusline_original, None);
}

#[test]
fn statusline_original_round_trips_verbatim() {
    // The stash is the user's own value in whatever shape their harness used, so it
    // must survive serialization byte-for-byte — including keys we never write.
    let raw = serde_json::json!({"type": "command", "command": "their-bar --wide", "padding": 2, "theirKey": ["a", 1]});
    let mut m = marker("embedded", None);
    m.statusline_original = Some(raw.clone());

    let bytes = serde_json::to_vec(&m).expect("marker must serialize");
    let back: Marker = serde_json::from_slice(&bytes).expect("round-tripped marker must parse");
    assert_eq!(back.statusline_original, Some(raw));
}

#[test]
fn an_absent_stash_writes_no_key() {
    // `skip_serializing_if` keeps a stash-free marker byte-identical to what a
    // pre-statusLine binary wrote, so an older binary reading it sees no change.
    let rendered = serde_json::to_string(&marker("embedded", None)).expect("marker must serialize");
    assert!(!rendered.contains("statusline_original"), "an empty stash must not emit the key: {rendered}");
}

#[test]
fn marker_without_statusline_command_still_deserializes() {
    // Every marker written before the slot's ownership record existed carries no such
    // key. It must load as "no record", which puts ownership back on the
    // current-command compare — not fail the whole marker read, which is treated as
    // absent and would silently re-adopt the install.
    let json = r#"{
        "binary_version": "0.1.0",
        "plugin_version": "0.1.0",
        "source_mode": "embedded",
        "scope": "user",
        "agent": "claude",
        "statusline_original": {"type": "command", "command": "their-bar"}
    }"#;
    let marker: Marker = serde_json::from_str(json).expect("an old marker without statusline_command must still load");
    assert_eq!(marker.statusline_command, None);
    assert!(marker.statusline_original.is_some(), "the stash beside it must still load");
}

#[test]
fn an_absent_statusline_command_writes_no_key() {
    // `skip_serializing_if` keeps a record-free marker byte-identical to what a binary
    // predating the field wrote, so an older binary reading it sees no change.
    let rendered = serde_json::to_string(&marker("embedded", None)).expect("marker must serialize");
    assert!(!rendered.contains("statusline_command"), "an unset command record must not emit the key: {rendered}");
}

#[test]
fn statusline_command_round_trips() {
    let mut m = marker("embedded", None);
    m.statusline_command = Some("mytool statusline --client claude".into());
    let bytes = serde_json::to_vec(&m).expect("marker must serialize");
    let back: Marker = serde_json::from_slice(&bytes).expect("round-tripped marker must parse");
    assert_eq!(back.statusline_command.as_deref(), Some("mytool statusline --client claude"));
}

#[test]
fn source_from_marker_prefers_a_path_mode_marker() {
    let m = marker("path", Some("/a/plugin"));
    assert_eq!(source_from_marker(Some(&m), Source::Embedded), Source::Path(PathBuf::from("/a/plugin")));
}

#[test]
fn source_from_marker_falls_back_to_default_when_marker_is_absent() {
    assert_eq!(source_from_marker(None, Source::Embedded), Source::Embedded);
}

#[test]
fn source_from_marker_rehydrates_an_embedded_marker() {
    // An explicit `install(Source::Embedded)` on a github-default host stays
    // embedded through repair/uninstall instead of drifting to `DEFAULT_SOURCE`
    // (which would make the github gate skip the backend and orphan its writes).
    let m = marker("embedded", None);
    assert_eq!(source_from_marker(Some(&m), Source::GitHub { repo: "o/r", ref_: "v1" }), Source::Embedded);
}

#[test]
fn source_from_marker_falls_back_to_default_for_a_github_marker() {
    // A github-mode marker carries no runtime source of its own; the compile-time
    // default already supplies the repo + ref.
    let m = marker("github", None);
    assert_eq!(source_from_marker(Some(&m), Source::GitHub { repo: "o/r", ref_: "v1" }), Source::GitHub { repo: "o/r", ref_: "v1" });
}

#[test]
fn source_from_marker_falls_back_when_path_mode_but_no_persisted_path() {
    // The pre-fix migration case: a `--path` install stamped before this field
    // existed has `source_mode == "path"` but `source_path: None` forever (until a
    // `setup --path` re-run refreshes it) — it must fall back to `default`, not
    // panic or synthesize an empty path.
    let m = marker("path", None);
    assert_eq!(source_from_marker(Some(&m), Source::Embedded), Source::Embedded);
}

#[test]
fn source_from_marker_is_per_marker_not_broadcast() {
    // Two different agents' markers resolve independently: one's path-mode marker
    // must never leak into a call made with the other's (embedded) marker. This is
    // the pure half of the per-agent-marker invariant `resolve_source`/callers rely
    // on — the I/O half (reading each agent's OWN marker, never a sibling's) is
    // proven by the host-fixture hermetic cross-agent test.
    let path_marker = marker("path", Some("/a/plugin"));
    let embedded_marker = marker("embedded", None);
    assert_eq!(source_from_marker(Some(&path_marker), Source::Embedded), Source::Path(PathBuf::from("/a/plugin")));
    assert_eq!(source_from_marker(Some(&embedded_marker), Source::Embedded), Source::Embedded);
}
