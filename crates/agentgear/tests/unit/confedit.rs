//! confedit tests: json_edit round-trips, preserves unknown keys, reports changed
//! vs unchanged, never creates an empty file, and BOM-free with a trailing newline;
//! write_file_idem + remove_file_idem idempotency; and the removal pair's prune rule
//! (only a container OUR removal emptied goes, never one the user had empty) plus its
//! drop of a file left holding nothing. Explicit temp paths, no env.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::{json_edit, json_obj_at, json_prune_at, json_prune_obj, json_remove, remove_file_idem, write_file_idem};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-confedit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn json_edit_creates_preserves_and_detects_change() {
    let path = scratch("cfg.json");
    // Seed an existing config with an unrelated key that must survive.
    std::fs::write(&path, br#"{"userKey":123,"mcpServers":{"theirs":{"command":"x"}}}"#).unwrap();

    let changed = json_edit(&path, |root| {
        json_obj_at(root, &["mcpServers"]).insert("ours".into(), json!({"command":"host"}));
        Ok(())
    })
    .unwrap();
    assert!(changed, "adding a server must report changed");

    let bytes = std::fs::read(&path).unwrap();
    let back: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back["userKey"], json!(123), "unknown top-level key survived");
    assert_eq!(back["mcpServers"]["theirs"]["command"], json!("x"), "user's server survived");
    assert_eq!(back["mcpServers"]["ours"]["command"], json!("host"));
    assert_ne!(bytes.first(), Some(&0xEF), "no UTF-8 BOM");
    assert_eq!(bytes.last(), Some(&b'\n'), "trailing newline");

    // A second identical edit is a byte-for-byte no-op.
    let before = std::fs::read(&path).unwrap();
    let changed = json_edit(&path, |root| {
        json_obj_at(root, &["mcpServers"]).insert("ours".into(), json!({"command":"host"}));
        Ok(())
    })
    .unwrap();
    assert!(!changed, "re-inserting the same value must report unchanged");
    assert_eq!(std::fs::read(&path).unwrap(), before, "unchanged edit must not rewrite");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn json_edit_missing_file_semantics() {
    let created = scratch("new.json");
    let changed = json_edit(&created, |root| {
        json_obj_at(root, &["mcp"]).insert("s".into(), json!({"command":"c"}));
        Ok(())
    })
    .unwrap();
    assert!(changed);
    assert!(created.exists());

    // A no-op edit on a missing file creates nothing.
    let untouched = scratch("noop.json");
    let changed = json_edit(&untouched, |_root| Ok(())).unwrap();
    assert!(!changed);
    assert!(!untouched.exists(), "a no-op edit must not create an empty file");

    std::fs::remove_dir_all(created.parent().unwrap()).ok();
    std::fs::remove_dir_all(untouched.parent().unwrap()).ok();
}

#[test]
fn json_edit_treats_empty_file_as_object() {
    // A touched / interrupted-write (empty or whitespace-only) config is not a parse
    // error; it starts from `{}` so an install still lands.
    let path = scratch("empty.json");
    std::fs::write(&path, b"   \n\t").unwrap();
    let changed = json_edit(&path, |root| {
        json_obj_at(root, &["mcpServers"]).insert("ours".into(), json!({"command":"host"}));
        Ok(())
    })
    .unwrap();
    assert!(changed);
    let back: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(back["mcpServers"]["ours"]["command"], json!("host"));
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

/// Drop `key` from the container at `path`, the shape every removal path has.
fn prune_drop(root: &mut Value, path: &[&str], key: &str) -> bool {
    json_prune_obj(root, path, |obj| {
        obj.remove(key);
        Ok(())
    })
    .unwrap()
}

#[test]
fn json_prune_drops_only_a_container_our_removal_emptied() {
    // Ours was the last key: the container we created goes with it.
    let mut root = json!({"theme":"dark","mcpServers":{"ours":{"command":"host"}}});
    assert!(prune_drop(&mut root, &["mcpServers"], "ours"));
    assert_eq!(root, json!({"theme":"dark"}), "the container our own key emptied must go");

    // A user key survives ours, so the container stays — with exactly theirs in it.
    let mut root = json!({"mcpServers":{"ours":{"command":"host"},"theirs":{"command":"x"}}});
    assert!(!prune_drop(&mut root, &["mcpServers"], "ours"));
    assert_eq!(root, json!({"mcpServers":{"theirs":{"command":"x"}}}), "a container still holding theirs must stay");

    // The user's own empty container: our removal takes nothing, so it is not ours to
    // prune. This is the whole difference between "we emptied it" and "it is empty".
    let mut root = json!({"theme":"dark","ui":{}});
    assert!(!prune_drop(&mut root, &["ui"], "statusLine"));
    assert_eq!(root, json!({"theme":"dark","ui":{}}), "a container the user had empty must survive untouched");

    // A missing container is navigated, never created.
    let mut root = json!({"theme":"dark"});
    assert!(!prune_drop(&mut root, &["ui"], "statusLine"));
    assert_eq!(root, json!({"theme":"dark"}), "a missing container must not be created by a removal");

    // Nested: dropping the leaf container takes the ancestor it emptied with it, and
    // stops at the level still holding something of the user's.
    let mut root = json!({"a":{"b":{"ours":1}}});
    assert!(prune_drop(&mut root, &["a", "b"], "ours"));
    assert_eq!(root, json!({}), "an ancestor emptied by the level we dropped must go too");
    let mut root = json!({"a":{"keep":1,"b":{"ours":1}}});
    assert!(prune_drop(&mut root, &["a", "b"], "ours"));
    assert_eq!(root, json!({"a":{"keep":1}}), "an ancestor still holding a user key must stay");

    // An emptied ARRAY is the same class (opencode's `instructions[]`).
    let mut root = json!({"instructions":["/ours.md"]});
    let pruned = json_prune_at(&mut root, &["instructions"], |list| {
        list.as_array_mut().unwrap().retain(|e| e != "/ours.md");
        Ok(())
    })
    .unwrap();
    assert!(pruned);
    assert_eq!(root, json!({}), "an array key our entry emptied must go");
}

#[test]
fn json_remove_drops_a_file_left_holding_nothing() {
    // Everything in it was ours, so the file is ours to take back.
    let ours = scratch("ours.json");
    std::fs::write(&ours, br#"{"mcpServers":{"ours":{"command":"host"}}}"#).unwrap();
    let changed = json_remove(&ours, |root| {
        prune_drop(root, &["mcpServers"], "ours");
        Ok(())
    })
    .unwrap();
    assert!(changed);
    assert!(!ours.exists(), "a root emptied by our own removal must take the file with it");

    // One user key left: the file is rewritten, never dropped.
    let shared = scratch("shared.json");
    std::fs::write(&shared, br#"{"theme":"dark","mcpServers":{"ours":{"command":"host"}}}"#).unwrap();
    let changed = json_remove(&shared, |root| {
        prune_drop(root, &["mcpServers"], "ours");
        Ok(())
    })
    .unwrap();
    assert!(changed);
    let back: Value = serde_json::from_slice(&std::fs::read(&shared).unwrap()).unwrap();
    assert_eq!(back, json!({"theme":"dark"}), "a root still holding a user key must survive as a file");

    // A teardown with nothing of ours to undo writes nothing — including against a
    // file the user deliberately keeps empty, and against no file at all.
    let empty = scratch("empty.json");
    std::fs::write(&empty, b"{}").unwrap();
    assert!(!json_remove(&empty, |_root| Ok(())).unwrap());
    assert_eq!(std::fs::read(&empty).unwrap(), b"{}", "a no-op teardown must not touch a user's empty config");
    let missing = scratch("missing.json");
    assert!(!json_remove(&missing, |_root| Ok(())).unwrap());
    assert!(!missing.exists(), "a no-op teardown must not create a file");

    // The inverse control: the very same emptying edit through `json_edit` keeps the
    // file. Dropping one is the removal pair's job alone, so no install path can.
    let install_side = scratch("install.json");
    std::fs::write(&install_side, br#"{"mcpServers":{"ours":{"command":"host"}}}"#).unwrap();
    let changed = json_edit(&install_side, |root| {
        prune_drop(root, &["mcpServers"], "ours");
        Ok(())
    })
    .unwrap();
    assert!(changed);
    let back: Value = serde_json::from_slice(&std::fs::read(&install_side).unwrap()).unwrap();
    assert_eq!(back, json!({}), "`json_edit` must never delete a file, however empty the edit leaves it");

    for p in [&ours, &shared, &empty, &missing, &install_side] {
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }
}

#[test]
fn write_and_remove_file_idem() {
    let path = scratch("f.txt");
    assert!(write_file_idem(&path, b"one").unwrap());
    assert!(!write_file_idem(&path, b"one").unwrap(), "same bytes must not rewrite");
    assert!(write_file_idem(&path, b"two").unwrap(), "changed bytes must rewrite");
    assert!(remove_file_idem(&path).unwrap(), "present file removal reports true");
    assert!(!remove_file_idem(&path).unwrap(), "absent file removal reports false");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
