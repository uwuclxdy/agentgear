//! claude backend ref-pinning pins: the github source string (`owner/repo@ref`)
//! and the present-branch re-point rule (matching ref → update, drifted → re-add).
//! No CLI/fs — the mutating calls are IO-only; these guard the decision that picks
//! which one to send. See docs/design.md §ref-pinning ground truth.

use std::path::PathBuf;

use super::{MarketplaceOp, github_source, marketplace_op};
use crate::host::Source;
use crate::manifest::MarketplaceEntry;

fn github(ref_: &'static str) -> Source {
    Source::GitHub { repo: "owner/repo", ref_ }
}

fn entry(ref_: Option<&str>) -> MarketplaceEntry {
    MarketplaceEntry { name: Some("m".into()), path: None, ref_: ref_.map(str::to_string) }
}

#[test]
fn github_source_carries_the_ref() {
    // The `@ref` is what makes the CLI check the tag out; a bare `owner/repo` would
    // silently track the default branch (design §ref-pinning ground truth).
    assert_eq!(github_source("owner/repo", "v0.1.0"), "owner/repo@v0.1.0");
}

#[test]
fn absent_marketplace_is_added() {
    assert_eq!(marketplace_op(&github("v0.1.0"), None), MarketplaceOp::Add);
    assert_eq!(marketplace_op(&Source::Embedded, None), MarketplaceOp::Add);
}

#[test]
fn matching_github_ref_updates() {
    // Same ref: `update` picks up a moved tag without re-pointing the pin.
    assert_eq!(marketplace_op(&github("v0.1.0"), Some(&entry(Some("v0.1.0")))), MarketplaceOp::Update);
}

#[test]
fn drifted_github_ref_re_adds() {
    // A version bump re-points via re-add, because `update` never moves a pin.
    assert_eq!(marketplace_op(&github("v0.1.1"), Some(&entry(Some("v0.1.0")))), MarketplaceOp::Add);
    // A previously-bare entry (no stored ref) also gets re-pointed to the pin.
    assert_eq!(marketplace_op(&github("v0.1.0"), Some(&entry(None))), MarketplaceOp::Add);
}

#[test]
fn present_non_github_updates() {
    // Embedded/path present-branch is unchanged: always `update`.
    assert_eq!(marketplace_op(&Source::Embedded, Some(&entry(None))), MarketplaceOp::Update);
    assert_eq!(marketplace_op(&Source::Path(PathBuf::from("/tmp/x")), Some(&entry(None))), MarketplaceOp::Update);
}
