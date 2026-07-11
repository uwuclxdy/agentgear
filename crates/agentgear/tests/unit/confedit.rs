//! confedit tests: json_edit round-trips, preserves unknown keys, reports changed
//! vs unchanged, never creates an empty file, and BOM-free with a trailing newline;
//! write_file_idem + remove_file_idem idempotency. Explicit temp paths, no env.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::{json_edit, json_obj_at, remove_file_idem, write_file_idem};

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
