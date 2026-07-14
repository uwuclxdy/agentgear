//! The derive's `embed = false` arm has no other compile site in the workspace —
//! every shipped host bakes a blob — so this target is that arm's only gate. It
//! proves the macro still expands with the attr off and that nothing is baked.
//! The lib half of the pairing (`--no-default-features`) is a CI leg, since every
//! other leg builds `--all-features`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agentgear::{PluginHost, Source};

/// A zero-embed host: no blob, so the lifecycle keys on the GitHub tag instead
/// (`default_source = "github"`, per the design's zero-embed recipe).
///
/// Compile-and-read-consts only: never call a lifecycle method on it. It reuses
/// `FixtureHost`'s plugin name (the derive cross-checks the name against
/// `plugin/plugin.json` at expansion), so both hosts resolve to one marketplace id
/// and an install here would fight the real fixture over it.
#[derive(PluginHost)]
#[plugin(name = "ez-fixture-plugin", embed = false, default_source = "github", github_repo = "uwuclxdy/agentgear")]
struct ZeroEmbedHost;

#[test]
fn bakes_no_blob() {
    assert!(ZeroEmbedHost::embedded_blob().is_empty(), "`embed = false` must bake an empty blob");
}

#[test]
fn defaults_to_the_version_tag_not_a_branch() {
    // The tree and the binary stay version-aligned: the ref is the tag `claude
    // plugin tag` produces, never a moving branch.
    let Source::GitHub { repo, ref_ } = ZeroEmbedHost::DEFAULT_SOURCE else {
        panic!("`default_source = \"github\"` must expand to Source::GitHub");
    };
    assert_eq!(repo, "uwuclxdy/agentgear");
    assert_eq!(ref_, concat!("v", env!("CARGO_PKG_VERSION")));
}
