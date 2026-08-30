//! Materialize unit tests: the tree-hash equivalence invariant (doctor step 5),
//! marketplace generation, idempotent version-dir writes, and the compress →
//! decompress roundtrip. All use explicit temp paths, so no process env is
//! mutated. Linked into `materialize.rs` (gated on the `embed` feature, which the
//! blob helpers need).

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{
    TreeSource, blob_entries, compress_dir, content_hash, dir_hash, expand_client_entries, generate_marketplace, prune_superseded,
    version_dir_name, write_version_dir,
};
use crate::host::Plugin;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/plugin");

fn test_plugin() -> Plugin {
    // `blob` is unused by the helpers under test (they take entries/blob directly).
    Plugin {
        name: "ez-test-plugin",
        marketplace: "ez-test-mkt",
        version: "0.1.0",
        agents: &["claude"],
        instructions: None,
        statusline: None,
        blob: &[],
    }
}

fn fixture_blob() -> Vec<u8> {
    compress_dir(Path::new(FIXTURE)).unwrap()
}

fn scratch() -> PathBuf {
    let dir = crate::scratch::path("ez-mat");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn embedded_hash_equals_materialized_hash() {
    let plugin = test_plugin();
    let blob = fixture_blob();
    let mut entries = blob_entries(&blob).unwrap();
    // Materialize as the CC client does (token-substituted); the fixture carries no
    // token, so this is a no-op and the equality is the same one doctor relies on.
    expand_client_entries(&mut entries, "claude");
    let root = scratch();
    let versions = root.join("versions");
    std::fs::create_dir_all(&versions).unwrap();
    let version_dir = versions.join("0.1.0@claude");

    write_version_dir(&plugin, &entries, &versions, &version_dir).unwrap();

    // The generated marketplace.json lives in the materialized tree but is
    // excluded from the hash, so the two sides must match exactly.
    assert!(version_dir.join(".claude-plugin/marketplace.json").exists());
    assert_eq!(dir_hash(&version_dir).unwrap(), content_hash(TreeSource::Blob(&blob), "claude").unwrap());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn client_token_materializes_per_client_and_hashes_match_baseline() {
    // A tree carrying ${AGENTGEAR_CLIENT} materializes with the token replaced by the
    // passed client id; the on-disk tree hashes equal to the client-scoped baseline
    // (the exact equality doctor's check_tree_hash keys on), and two clients differ.
    let src = scratch();
    let cp = src.join(".claude-plugin");
    std::fs::create_dir_all(&cp).unwrap();
    std::fs::write(cp.join("plugin.json"), br#"{"name":"tok","version":"0.1.0","description":"d","author":{"name":"a"}}"#).unwrap();
    std::fs::create_dir_all(src.join("hooks")).unwrap();
    std::fs::write(
        src.join("hooks").join("hooks.json"),
        br#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"h --client ${AGENTGEAR_CLIENT}"}]}]}}"#,
    )
    .unwrap();

    let blob = compress_dir(&src).unwrap();
    let plugin = Plugin {
        name: "tok",
        marketplace: "tok-mkt",
        version: "0.1.0",
        agents: &["claude"],
        instructions: None,
        statusline: None,
        blob: &[],
    };

    let root = scratch();
    let versions = root.join("versions");
    std::fs::create_dir_all(&versions).unwrap();
    let mut entries = blob_entries(&blob).unwrap();
    expand_client_entries(&mut entries, "claude");
    let version_dir = versions.join("0.1.0@claude");
    write_version_dir(&plugin, &entries, &versions, &version_dir).unwrap();

    // The token was substituted on disk, not left raw.
    let h = std::fs::read_to_string(version_dir.join("hooks/hooks.json")).unwrap();
    assert!(h.contains("--client claude"), "token not substituted:\n{h}");
    assert!(!h.contains("${AGENTGEAR_CLIENT}"), "raw token survived on disk:\n{h}");

    // doctor's exact equality: on-disk current@claude == the substituted baseline,
    // from both the blob and the source-dir hashers.
    assert_eq!(dir_hash(&version_dir).unwrap(), content_hash(TreeSource::Blob(&blob), "claude").unwrap());
    assert_eq!(dir_hash(&version_dir).unwrap(), content_hash(TreeSource::Dir(&src), "claude").unwrap());
    // Per-client staging: a different client bakes different bytes, so the shared
    // data root can never collide between two plugin-native backends.
    assert_ne!(content_hash(TreeSource::Blob(&blob), "claude").unwrap(), content_hash(TreeSource::Blob(&blob), "copilot-cli").unwrap());

    std::fs::remove_dir_all(&src).ok();
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn write_version_dir_is_idempotent_when_target_exists() {
    let plugin = test_plugin();
    let entries = blob_entries(&fixture_blob()).unwrap();
    let root = scratch();
    let versions = root.join("versions");
    std::fs::create_dir_all(&versions).unwrap();
    let version_dir = versions.join("0.1.0@claude");

    write_version_dir(&plugin, &entries, &versions, &version_dir).unwrap();
    let first = dir_hash(&version_dir).unwrap();
    // Simulate the lost-race path: target already present. Must not error and
    // must leave the existing tree intact.
    write_version_dir(&plugin, &entries, &versions, &version_dir).unwrap();
    assert_eq!(dir_hash(&version_dir).unwrap(), first);
    // No leftover temp dirs.
    let temps: Vec<_> = std::fs::read_dir(&versions)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(temps.is_empty(), "leftover temp dirs: {temps:?}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn one_changed_byte_at_an_unchanged_version_keys_a_new_dir() {
    // The staleness class: version-keyed dirs skipped the write for any tree the
    // version had already staged, so a same-version edit never reached a box.
    let src = scratch();
    let cp = src.join(".claude-plugin");
    std::fs::create_dir_all(&cp).unwrap();
    std::fs::write(cp.join("plugin.json"), br#"{"name":"drift","version":"0.1.0","description":"d","author":{"name":"a"}}"#).unwrap();
    std::fs::create_dir_all(src.join("hooks")).unwrap();
    std::fs::write(src.join("hooks").join("hooks.json"), b"{\"hooks\":{}}").unwrap();

    let before = content_hash(TreeSource::Dir(&src), "claude").unwrap();
    std::fs::write(src.join("hooks").join("hooks.json"), b"{\"hooks\":{ }}").unwrap();
    let after = content_hash(TreeSource::Dir(&src), "claude").unwrap();

    assert_ne!(before, after, "a changed byte must change the tree hash");
    assert_ne!(
        version_dir_name("0.1.0", &before, "claude"),
        version_dir_name("0.1.0", &after, "claude"),
        "a changed tree must key a different version dir at the same version"
    );

    std::fs::remove_dir_all(&src).ok();
}

#[test]
fn prune_drops_this_versions_other_variants_and_keeps_everything_else() {
    let versions = scratch();
    let keep = version_dir_name("0.1.0", &"a".repeat(64), "claude");
    // Superseded: this version's other content variant, plus the version-keyed name a
    // pre-content-keying binary wrote.
    let superseded = version_dir_name("0.1.0", &"b".repeat(64), "claude");
    let legacy = "0.1.0@claude".to_string();
    // Kept: another client's staging, another version, a pre-release version this
    // version's string is a prefix of, and a concurrent writer's temp dir.
    let other_client = version_dir_name("0.1.0", &"c".repeat(64), "copilot-cli");
    let other_version = version_dir_name("0.2.0", &"d".repeat(64), "claude");
    let prerelease = version_dir_name("0.1.0-rc.1", &"e".repeat(64), "claude");
    let temp = "0.1.0.tmp.deadbeef.42".to_string();
    let all = [&keep, &superseded, &legacy, &other_client, &other_version, &prerelease, &temp];
    for name in all {
        std::fs::create_dir_all(versions.join(name)).unwrap();
    }

    prune_superseded(&versions, "0.1.0", "claude", &keep);

    for name in [&superseded, &legacy] {
        assert!(!versions.join(name).exists(), "{name} should have been pruned");
    }
    for name in [&keep, &other_client, &other_version, &prerelease, &temp] {
        assert!(versions.join(name).exists(), "{name} must survive the prune");
    }

    std::fs::remove_dir_all(&versions).ok();
}

#[test]
fn generated_marketplace_has_no_version_and_no_bom() {
    let plugin = test_plugin();
    let entries = blob_entries(&fixture_blob()).unwrap();
    let bytes = generate_marketplace(&plugin, &entries).unwrap();

    assert_ne!(bytes.first(), Some(&0xEF), "must not start with a UTF-8 BOM");

    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["name"], "ez-test-mkt");
    assert!(value.get("version").is_none(), "marketplace entry must not carry a version");
    assert!(value["description"].is_string());
    assert!(value["owner"]["name"].is_string());
    assert_eq!(value["plugins"][0]["source"], "./");
    assert_eq!(value["plugins"][0]["name"], "ez-test-plugin");
}

#[test]
fn blob_roundtrips_and_compresses_repetitive_input() {
    // A synthetic tree with a large, compressible file exercises the codec end to
    // end: the blob must be materially smaller than the raw bytes and decompress
    // back to the exact input (the fixture is too small to show a ratio).
    let dir = scratch();
    let cp = dir.join(".claude-plugin");
    std::fs::create_dir_all(&cp).unwrap();
    std::fs::write(cp.join("plugin.json"), br#"{"name":"x","version":"0.1.0","description":"d","author":{"name":"a"}}"#).unwrap();
    let big = "abcdefgh".repeat(8192); // 64 KiB, highly compressible
    std::fs::write(dir.join("big.txt"), big.as_bytes()).unwrap();

    let blob = compress_dir(&dir).unwrap();
    assert!(blob.len() * 4 < big.len(), "blob {} not materially smaller than raw {}", blob.len(), big.len());

    let entries = blob_entries(&blob).unwrap();
    let got = entries.iter().find(|(rel, _)| rel == "big.txt").map(|(_, b)| b.clone()).unwrap();
    assert_eq!(got, big.as_bytes());

    std::fs::remove_dir_all(&dir).ok();
}
