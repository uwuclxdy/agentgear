//! `cc_config_dir` unit tests: the pure env-resolution core behind the claude
//! classifier's config-dir lookup, split so its three arms are testable without
//! mutating process env (edition-2024 `set_var` ban).

use std::ffi::OsStr;
use std::path::PathBuf;

use super::{CcConfigDir, cc_config_dir_from};

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
