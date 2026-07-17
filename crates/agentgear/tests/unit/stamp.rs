//! stamp unit tests: pure `Marker` (de)serialization only. `read`/`write`/`clear`
//! (and `resolve_source`, which calls `read`) route through `data_root`
//! (`dirs::data_dir`), which can't be redirected without mutating process env —
//! forbidden in edition 2024 — so the marker lifecycle, path-source rehydration
//! included, is covered by the host-fixture e2e (matching how `stamp.rs` is
//! tested today).

use crate::stamp::Marker;

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
