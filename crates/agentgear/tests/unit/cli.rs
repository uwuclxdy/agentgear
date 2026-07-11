//! Version-parse + monotonic-compare unit tests. Linked into `cli.rs`.

use super::{parse_version, version_lt};

#[test]
fn parses_plain_semver() {
    assert_eq!(parse_version("2.1.201"), Some((2, 1, 201)));
    assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
}

#[test]
fn parses_version_with_trailing_label() {
    // `claude --version` prints e.g. "2.1.201 (Claude Code)".
    assert_eq!(parse_version("2.1.201 (Claude Code)"), Some((2, 1, 201)));
    // trailing pre-release on the patch is tolerated (leading digits only)
    assert_eq!(parse_version("1.2.3-beta"), Some((1, 2, 3)));
}

#[test]
fn rejects_garbage() {
    assert_eq!(parse_version(""), None);
    assert_eq!(parse_version("nightly"), None);
    assert_eq!(parse_version("2.x"), None);
}

#[test]
fn version_lt_is_monotonic_and_safe() {
    assert!(version_lt(Some("0.1.0"), "0.2.0"));
    assert!(version_lt(Some("2.1.195"), "2.1.196"));
    assert!(!version_lt(Some("0.2.0"), "0.1.0")); // never downgrade
    assert!(!version_lt(Some("0.1.0"), "0.1.0")); // equal is not older
    // unparseable or missing installed => never churn
    assert!(!version_lt(None, "0.1.0"));
    assert!(!version_lt(Some("weird"), "0.1.0"));
}
