//! `registry_lists_plugin` unit tests: the HOME-based `installed_plugins.json`
//! reader shared by omp's `claude-plugins` provider gate and cursor's
//! `loadClaude`-coverage gate.

use std::path::PathBuf;

use super::registry_lists_plugin;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-ccregistry-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn registry_lists_plugin_requires_version_key_and_a_non_empty_entry() {
    let path = scratch("installed_plugins.json");
    let id = "ez-fixture-plugin@ez-fixture-plugin";

    // Absent file -> not covering (we translate).
    assert!(!registry_lists_plugin(&path, id));

    // A version-less registry (every native consumer's own parser treats this as absent).
    std::fs::write(&path, r#"{"plugins":{"ez-fixture-plugin@ez-fixture-plugin":[{"installPath":"x"}]}}"#).unwrap();
    assert!(!registry_lists_plugin(&path, id), "a registry without a numeric version key must read as absent");

    // Version present but our id missing / its entry empty -> not listed.
    std::fs::write(&path, r#"{"version":1,"plugins":{"other@mkt":[{"installPath":"x"}]}}"#).unwrap();
    assert!(!registry_lists_plugin(&path, id));
    std::fs::write(&path, r#"{"version":1,"plugins":{"ez-fixture-plugin@ez-fixture-plugin":[]}}"#).unwrap();
    assert!(!registry_lists_plugin(&path, id), "an empty install array is not a live entry");

    // Version + a non-empty install entry -> listed.
    std::fs::write(&path, r#"{"version":1,"plugins":{"ez-fixture-plugin@ez-fixture-plugin":[{"scope":"user","installPath":"x"}]}}"#)
        .unwrap();
    assert!(registry_lists_plugin(&path, id));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
