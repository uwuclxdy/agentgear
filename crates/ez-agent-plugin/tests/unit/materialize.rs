//! Materialize unit tests: the tree-hash equivalence invariant (doctor step 5),
//! marketplace generation, and idempotent version-dir writes. All use explicit
//! temp paths, so no process env is mutated. Linked into `materialize.rs`.

use include_dir::{Dir, include_dir};
use serde_json::Value;

use super::{dir_hash, generate_marketplace, tree_hash, write_version_dir};
use crate::host::Plugin;

static TREE: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/plugin");

fn test_plugin() -> Plugin {
    Plugin { name: "ez-test-plugin", marketplace: "ez-test-mkt", version: "0.1.0", agents: &["claude"], tree: &TREE }
}

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-mat-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn embedded_hash_equals_materialized_hash() {
    let plugin = test_plugin();
    let root = scratch();
    let versions = root.join("versions");
    std::fs::create_dir_all(&versions).unwrap();
    let version_dir = versions.join("0.1.0");

    write_version_dir(&plugin, &versions, &version_dir).unwrap();

    // The generated marketplace.json lives in the materialized tree but is
    // excluded from the hash, so the two sides must match exactly.
    assert!(version_dir.join(".claude-plugin/marketplace.json").exists());
    assert_eq!(dir_hash(&version_dir).unwrap(), tree_hash(plugin.tree()));

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn write_version_dir_is_idempotent_when_target_exists() {
    let plugin = test_plugin();
    let root = scratch();
    let versions = root.join("versions");
    std::fs::create_dir_all(&versions).unwrap();
    let version_dir = versions.join("0.1.0");

    write_version_dir(&plugin, &versions, &version_dir).unwrap();
    let first = dir_hash(&version_dir).unwrap();
    // Simulate the lost-race path: target already present. Must not error and
    // must leave the existing tree intact.
    write_version_dir(&plugin, &versions, &version_dir).unwrap();
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
fn generated_marketplace_has_no_version_and_no_bom() {
    let plugin = test_plugin();
    let bytes = generate_marketplace(&plugin).unwrap();

    assert_ne!(bytes.first(), Some(&0xEF), "must not start with a UTF-8 BOM");

    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["name"], "ez-test-mkt");
    assert!(value.get("version").is_none(), "marketplace entry must not carry a version");
    assert!(value["description"].is_string());
    assert!(value["owner"]["name"].is_string());
    assert_eq!(value["plugins"][0]["source"], "./");
    assert_eq!(value["plugins"][0]["name"], "ez-test-plugin");
}
