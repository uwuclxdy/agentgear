//! claude backend ref-pinning pins: the github source string (`owner/repo@ref`)
//! and the present-branch re-point rule (matching ref → update, drifted → re-add).
//! No CLI/fs — the mutating calls are IO-only; these guard the decision that picks
//! which one to send. See docs/design.md §ref-pinning ground truth.

use std::path::PathBuf;

use super::{MarketplaceHealth, MarketplaceOp, github_source, marketplace_health, marketplace_op};
use crate::host::Source;
use crate::manifest::MarketplaceEntry;

fn github(ref_: &'static str) -> Source {
    Source::GitHub { repo: "owner/repo", ref_ }
}

fn entry(ref_: Option<&str>) -> MarketplaceEntry {
    MarketplaceEntry { name: Some("m".into()), path: None, ref_: ref_.map(str::to_string) }
}

fn entry_with_path(path: &str) -> MarketplaceEntry {
    MarketplaceEntry { name: Some("m".into()), path: Some(path.into()), ref_: None }
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

// marketplace_health: the classifier doctor's marketplace check and reconcile's
// structural_ok share. GitHub has no local path to dangle; a missing `path` reads
// as healthy (tolerant serde); a present-but-nonexistent local path dangles.

#[test]
fn marketplace_health_absent_entry() {
    assert_eq!(marketplace_health(None, &Source::Embedded), MarketplaceHealth::Absent);
    assert_eq!(marketplace_health(None, &github("v0.1.0")), MarketplaceHealth::Absent);
    assert_eq!(marketplace_health(None, &Source::Path(PathBuf::from("/tmp/x"))), MarketplaceHealth::Absent);
}

#[test]
fn marketplace_health_github_present_never_dangles() {
    // No local path field to go missing: the registry stores source + ref.
    assert_eq!(marketplace_health(Some(&entry(None)), &github("v0.1.0")), MarketplaceHealth::Healthy);
}

#[test]
fn marketplace_health_missing_path_field_is_healthy() {
    // A present local entry with no `path` is tolerant-read as healthy.
    assert_eq!(marketplace_health(Some(&entry(None)), &Source::Embedded), MarketplaceHealth::Healthy);
}

#[test]
fn marketplace_health_local_path_resolves_is_healthy() {
    let existing = std::env::temp_dir();
    let e = MarketplaceEntry { name: Some("m".into()), path: Some(existing.to_string_lossy().into_owned()), ref_: None };
    assert_eq!(marketplace_health(Some(&e), &Source::Embedded), MarketplaceHealth::Healthy);
    assert_eq!(marketplace_health(Some(&e), &Source::Path(PathBuf::from("/tmp/x"))), MarketplaceHealth::Healthy);
}

#[test]
fn marketplace_health_local_path_missing_dangles() {
    let nonexistent = "/agentgear/marketplace-health/does/not/exist";
    assert!(!PathBuf::from(nonexistent).exists(), "fixture path must not exist for this assertion");
    assert_eq!(marketplace_health(Some(&entry_with_path(nonexistent)), &Source::Embedded), MarketplaceHealth::Dangling);
    assert_eq!(
        marketplace_health(Some(&entry_with_path(nonexistent)), &Source::Path(PathBuf::from("/tmp/x"))),
        MarketplaceHealth::Dangling
    );
}
