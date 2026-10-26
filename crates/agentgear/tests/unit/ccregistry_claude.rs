//! `cc_config_dir` unit tests: the pure env-resolution core behind the claude
//! classifier's config-dir lookup, split so its three arms are testable without
//! mutating process env (edition-2024 `set_var` ban). Plus the settings-declaration
//! drop a github re-point runs before `marketplace add`: only our own marketplace's
//! entry goes, only when it does not already describe the desired source, and a file
//! that cannot be parsed is refused rather than clobbered.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{CcConfigDir, cc_config_dir_from, drop_marketplace_declaration};

#[test]
fn non_empty_override_wins_over_home() {
    let dir = cc_config_dir_from(Some(OsStr::new("/explicit/dir")), Some(PathBuf::from("/home/someone")));
    assert_eq!(dir, CcConfigDir::Dir(PathBuf::from("/explicit/dir")));
}

#[test]
fn empty_override_is_cannot_tell() {
    let dir = cc_config_dir_from(Some(OsStr::new("")), Some(PathBuf::from("/home/someone")));
    assert_eq!(dir, CcConfigDir::EmptyOverride, "CC reads an empty override as the cwd, so a file reader cannot tell");
}

#[test]
fn no_override_falls_back_to_home_dot_claude() {
    let dir = cc_config_dir_from(None, Some(PathBuf::from("/home/someone")));
    assert_eq!(dir, CcConfigDir::Dir(PathBuf::from("/home/someone/.claude")));
}

#[test]
fn no_override_and_no_home_is_no_home() {
    let dir = cc_config_dir_from(None, None);
    assert_eq!(dir, CcConfigDir::NoHome);
}

const MARKETPLACE: &str = "our-mkt";
const REPO: &str = "owner/repo";
const REF: &str = "v2.0.0";

fn config_dir(name: &str) -> PathBuf {
    let dir = crate::scratch::path(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn settings_path(dir: &Path) -> PathBuf {
    dir.join("settings.json")
}

fn settings(dir: &Path) -> String {
    std::fs::read_to_string(settings_path(dir)).unwrap()
}

fn write_settings(dir: &Path, doc: &Value) {
    std::fs::write(settings_path(dir), serde_json::to_vec_pretty(doc).unwrap()).unwrap();
}

fn github(repo: &str, ref_: &str) -> Value {
    json!({"source": "github", "repo": repo, "ref": ref_})
}

/// A settings document whose `extraKnownMarketplaces` holds exactly `entries`.
fn settings_with(entries: Vec<(&str, Value)>) -> Value {
    let mut declarations = serde_json::Map::new();
    for (name, declaration) in entries {
        declarations.insert(name.to_string(), json!({"source": declaration}));
    }
    json!({"extraKnownMarketplaces": declarations})
}

#[test]
fn stale_github_declaration_is_dropped_and_other_settings_survive() {
    let dir = config_dir("ez-ccdecl-stale");
    let mut doc = settings_with(vec![(MARKETPLACE, github(REPO, "v1.0.0")), ("their-mkt", github("them/other", "v9"))]);
    doc["theirSetting"] = json!(true);
    doc["enabledPlugins"] = json!({"our-plugin@our-mkt": true});
    write_settings(&dir, &doc);

    let changed = drop_marketplace_declaration(&dir, MARKETPLACE, REPO, REF).unwrap();

    assert!(changed, "a declaration naming the old ref must be dropped");
    let after: Value = serde_json::from_str(&settings(&dir)).unwrap();
    assert_eq!(after["theirSetting"], true, "an unrelated top-level key must survive");
    assert_eq!(after["enabledPlugins"], json!({"our-plugin@our-mkt": true}), "the enabled set must survive");
    assert_eq!(
        after["extraKnownMarketplaces"].as_object().map(serde_json::Map::len),
        Some(1),
        "only our marketplace's entry goes: {after}"
    );
    assert_eq!(after["extraKnownMarketplaces"]["their-mkt"], json!({"source": github("them/other", "v9")}));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn matching_github_declaration_leaves_the_file_byte_identical() {
    let dir = config_dir("ez-ccdecl-match");
    write_settings(&dir, &settings_with(vec![(MARKETPLACE, github(REPO, REF))]));
    let before = settings(&dir);

    let changed = drop_marketplace_declaration(&dir, MARKETPLACE, REPO, REF).unwrap();

    assert!(!changed, "an entry that already declares the desired source must not be rewritten");
    assert_eq!(settings(&dir), before);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn declaration_for_another_repo_is_dropped() {
    let dir = config_dir("ez-ccdecl-repo");
    write_settings(&dir, &settings_with(vec![(MARKETPLACE, github("someone/fork", REF))]));

    let changed = drop_marketplace_declaration(&dir, MARKETPLACE, REPO, REF).unwrap();

    assert!(changed, "the same ref on another repo is still another source");
    let after: Value = serde_json::from_str(&settings(&dir)).unwrap();
    assert_eq!(after, json!({}), "the entry was the only key, so the container goes with it");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn directory_declaration_is_dropped() {
    let dir = config_dir("ez-ccdecl-dir");
    write_settings(&dir, &settings_with(vec![(MARKETPLACE, json!({"source": "directory", "path": "/gone"}))]));

    let changed = drop_marketplace_declaration(&dir, MARKETPLACE, REPO, REF).unwrap();

    assert!(changed, "a local-source declaration cannot serve a github re-add");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn declaration_with_an_extra_field_is_dropped() {
    let dir = config_dir("ez-ccdecl-extra");
    write_settings(&dir, &settings_with(vec![(MARKETPLACE, json!({"source": "github", "repo": REPO, "ref": REF, "name": "ours"}))]));

    let changed = drop_marketplace_declaration(&dir, MARKETPLACE, REPO, REF).unwrap();

    assert!(changed, "an extra field is a source shape the add does not send, so it must be re-declared");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_settings_file_stays_missing() {
    let dir = config_dir("ez-ccdecl-missing");

    let changed = drop_marketplace_declaration(&dir, MARKETPLACE, REPO, REF).unwrap();

    assert!(!changed, "there is no declaration to drop");
    assert!(!settings_path(&dir).exists(), "a drop must never create a settings file");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn empty_declaration_container_of_the_users_own_survives() {
    let dir = config_dir("ez-ccdecl-empty");
    write_settings(&dir, &json!({"extraKnownMarketplaces": {}}));
    let before = settings(&dir);

    let changed = drop_marketplace_declaration(&dir, MARKETPLACE, REPO, REF).unwrap();

    assert!(!changed, "a container holding none of our entries is the user's own");
    assert_eq!(settings(&dir), before);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unparseable_settings_is_refused_not_clobbered() {
    let dir = config_dir("ez-ccdecl-bad");
    std::fs::write(settings_path(&dir), b"{ not json").unwrap();

    let err = drop_marketplace_declaration(&dir, MARKETPLACE, REPO, REF).unwrap_err();

    assert!(matches!(err, crate::Error::Config { .. }), "a settings file we cannot parse must be refused, got {err:?}");
    assert_eq!(settings(&dir), "{ not json", "the refused file must survive byte-identical");
    std::fs::remove_dir_all(&dir).ok();
}
