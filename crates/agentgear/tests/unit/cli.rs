//! Env-scrub + version-parse + monotonic-compare unit tests. Linked into `cli.rs`.

use super::{parse_version, scrub_keys, version_lt};

fn scrub(vars: &[&str]) -> Vec<String> {
    scrub_keys(vars.iter().map(|key| (*key).to_string()))
}

#[test]
fn scrub_drops_the_session_env_and_keeps_the_config_dir() {
    let keys =
        scrub(&["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SSE_PORT", "CLAUDE_CONFIG_DIR", "CLAUDE_OTHER", "PATH", "HOME"]);
    assert_eq!(keys, ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SSE_PORT"]);
}

#[test]
fn scrub_drops_claudecode_even_when_the_parent_has_none() {
    // Unconditional: a child must never see a session marker, so the key is removed
    // whether or not this process carries it.
    assert_eq!(scrub(&[]), ["CLAUDECODE"]);
    assert_eq!(scrub(&["PATH"]), ["CLAUDECODE"]);
}

#[test]
fn scrub_matches_the_prefix_exactly() {
    // The trailing underscore is load-bearing: `CLAUDE_CODEX` is another tool's var,
    // not a CC session var, and a mid-string match is not a prefix.
    assert_eq!(scrub(&["CLAUDE_CODEX", "XCLAUDE_CODE_FOO", "MY_CLAUDECODE"]), ["CLAUDECODE"]);
}

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
