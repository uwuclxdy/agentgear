//! claude backend pure decisions: the github source string (`owner/repo@ref`),
//! the present-branch re-point rule (matching ref → update, drifted ref or
//! diverged local path → re-add), and the marketplace-health classifier the
//! probe, reconcile, and doctor share. See docs/design.md §ref-pinning ground
//! truth and §marketplace ground truth.
//!
//! No CLI here — the mutating calls are IO-only; these guard the decisions that
//! pick what they send.

use std::path::{Path, PathBuf};

use super::{MarketplaceHealth, MarketplaceOp, github_source, marketplace_health, marketplace_op};
use crate::host::{Scope, Source};
use crate::manifest::MarketplaceEntry;

const EXPECTED: &str = "/data/root/clauth/current@claude";

fn github(ref_: &'static str) -> Source {
    Source::GitHub { repo: "owner/repo", ref_ }
}

fn entry(ref_: Option<&str>) -> MarketplaceEntry {
    MarketplaceEntry { name: Some("m".into()), path: None, ref_: ref_.map(str::to_string), source: None }
}

fn entry_with_path(path: &str) -> MarketplaceEntry {
    MarketplaceEntry { name: Some("m".into()), path: Some(path.into()), ref_: None, source: Some("directory".into()) }
}

fn github_entry(ref_: Option<&str>) -> MarketplaceEntry {
    MarketplaceEntry { name: Some("m".into()), path: None, ref_: ref_.map(str::to_string), source: Some("github".into()) }
}

#[test]
fn github_source_carries_the_ref() {
    // The `@ref` is what makes the CLI check the tag out; a bare `owner/repo` would
    // silently track the default branch (design §ref-pinning ground truth).
    assert_eq!(github_source("owner/repo", "v0.1.0"), "owner/repo@v0.1.0");
}

#[test]
fn absent_marketplace_is_added() {
    assert_eq!(marketplace_op(&github("v0.1.0"), None, EXPECTED), MarketplaceOp::Add);
    assert_eq!(marketplace_op(&Source::Embedded, None, EXPECTED), MarketplaceOp::Add);
}

#[test]
fn matching_github_ref_updates() {
    // Same ref: `update` picks up a moved tag without re-pointing the pin.
    assert_eq!(marketplace_op(&github("v0.1.0"), Some(&github_entry(Some("v0.1.0"))), EXPECTED), MarketplaceOp::Update);
}

#[test]
fn drifted_github_ref_re_adds() {
    // A version bump re-points via re-add, because `update` never moves a pin.
    assert_eq!(marketplace_op(&github("v0.1.1"), Some(&github_entry(Some("v0.1.0"))), EXPECTED), MarketplaceOp::Add);
    // A previously-bare entry (no stored ref) also gets re-pointed to the pin.
    assert_eq!(marketplace_op(&github("v0.1.0"), Some(&github_entry(None)), EXPECTED), MarketplaceOp::Add);
}

#[test]
fn local_source_on_the_expected_pointer_updates() {
    // A directory entry registered exactly where materialize staged the tree is
    // the steady state: `update` refreshes the live copy.
    assert_eq!(marketplace_op(&Source::Embedded, Some(&entry_with_path(EXPECTED)), EXPECTED), MarketplaceOp::Update);
    assert_eq!(marketplace_op(&Source::Path(PathBuf::from("/tmp/x")), Some(&entry_with_path(EXPECTED)), EXPECTED), MarketplaceOp::Update);
}

#[test]
fn local_source_diverged_from_the_pointer_re_adds() {
    // The migration trigger: an entry registered at an old checkout dir (or a
    // pre-client-scoping `current` pointer) must be re-pointed — `update` only
    // re-fetches the stale source, and fails on it.
    assert_eq!(marketplace_op(&Source::Embedded, Some(&entry_with_path("/old/checkout/plugins")), EXPECTED), MarketplaceOp::Add);
}

#[test]
fn github_registered_entry_re_adds_under_a_local_source() {
    // Old github-sourced registrations carry no path; under an embedded host the
    // entry's own `source` is the divergence signal. `add <dir>` re-points it.
    // The `source` disjunct is an EQUIVALENT MUTANT on every probed input — every
    // real github entry also fails the path comparison (no `path` field) — kept
    // as a structural encoding of that probe fact; do not re-run a kill for it.
    assert_eq!(marketplace_op(&Source::Embedded, Some(&github_entry(None)), EXPECTED), MarketplaceOp::Add);
}

// marketplace_health: the classifier doctor's marketplace check and reconcile's
// structural_ok share. GitHub has no local path to dangle; under a github DESIRED
// source a present entry is always healthy. Under a local desired source, healthy
// means: a directory entry, registered at the materialized pointer, whose
// generated manifest exists — anything else is a heal's re-point job.

#[test]
fn marketplace_health_absent_entry() {
    assert_eq!(marketplace_health(None, &Source::Embedded, Path::new(EXPECTED)), MarketplaceHealth::Absent);
    assert_eq!(marketplace_health(None, &github("v0.1.0"), Path::new(EXPECTED)), MarketplaceHealth::Absent);
    assert_eq!(marketplace_health(None, &Source::Path(PathBuf::from("/tmp/x")), Path::new(EXPECTED)), MarketplaceHealth::Absent);
}

#[test]
fn marketplace_health_github_entry_healthy_under_github_source() {
    assert_eq!(marketplace_health(Some(&github_entry(Some("v0.1.0"))), &github("v0.1.0"), Path::new(EXPECTED)), MarketplaceHealth::Healthy);
}

#[test]
fn marketplace_health_github_entry_dangles_under_a_local_source() {
    // The same entry met by an embedded host is divergence, not health. Contract
    // pin, not a kill pin: the `source` arm is an equivalent mutant on probed
    // inputs (a github entry also fails the path-missing arm), recorded so it is
    // not re-attempted as a coverage gap.
    assert_eq!(marketplace_health(Some(&github_entry(None)), &Source::Embedded, Path::new(EXPECTED)), MarketplaceHealth::Dangling);
}

#[test]
fn marketplace_health_missing_path_field_dangles() {
    // A directory entry with no `path` cannot be the materialized pointer, so it
    // is never healthy under a local desired source.
    assert_eq!(marketplace_health(Some(&entry(None)), &Source::Embedded, Path::new(EXPECTED)), MarketplaceHealth::Dangling);
}

#[test]
fn marketplace_health_local_path_on_the_pointer_with_manifest_is_healthy() {
    let root = crate::scratch::path("ez-marketplace-health-healthy");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
    std::fs::write(root.join(".claude-plugin").join("marketplace.json"), "{}").unwrap();
    let e = MarketplaceEntry {
        name: Some("m".into()),
        path: Some(root.to_string_lossy().into_owned()),
        ref_: None,
        source: Some("directory".into()),
    };
    assert_eq!(marketplace_health(Some(&e), &Source::Embedded, &root), MarketplaceHealth::Healthy);
    assert_eq!(marketplace_health(Some(&e), &Source::Path(PathBuf::from("/tmp/x")), &root), MarketplaceHealth::Healthy);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn marketplace_health_local_path_missing_dangles() {
    let nonexistent = "/agentgear/marketplace-health/does/not/exist";
    assert!(!PathBuf::from(nonexistent).exists(), "fixture path must not exist for this assertion");
    assert_eq!(
        marketplace_health(Some(&entry_with_path(nonexistent)), &Source::Embedded, Path::new(EXPECTED)),
        MarketplaceHealth::Dangling
    );
}

#[test]
fn marketplace_health_diverged_path_dangles() {
    // Registered elsewhere than the pointer: the dir may resolve and even hold a
    // manifest, but the heal must re-point it — that is the migration case.
    let elsewhere = crate::scratch::path("ez-marketplace-health-elsewhere");
    let _ = std::fs::remove_dir_all(&elsewhere);
    std::fs::create_dir_all(elsewhere.join(".claude-plugin")).unwrap();
    std::fs::write(elsewhere.join(".claude-plugin").join("marketplace.json"), "{}").unwrap();
    assert_eq!(
        marketplace_health(Some(&entry_with_path(&elsewhere.to_string_lossy())), &Source::Embedded, Path::new(EXPECTED)),
        MarketplaceHealth::Dangling
    );
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn marketplace_health_manifest_missing_dangles() {
    // The exact deadlock the heal gate exists for: the registered path resolves
    // but its manifest was deleted, so CC serves 0 hooks and 0 MCP from it.
    let root = crate::scratch::path("ez-marketplace-health-no-manifest");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
    assert_eq!(marketplace_health(Some(&entry_with_path(&root.to_string_lossy())), &Source::Embedded, &root), MarketplaceHealth::Dangling);
    let _ = std::fs::remove_dir_all(&root);
}

// at_scope: the scope filter find_plugin keys on, so a user-scope op never reads
// a project entry first (the dead-entry shape a per-session config dir leaves).

fn entry_at(scope: &str) -> crate::manifest::PluginEntry {
    crate::manifest::PluginEntry {
        id: "clauth@clauth".into(),
        version: Some("0.14.1".into()),
        enabled: None,
        install_path: None,
        errors: None,
        scope: Some(scope.into()),
    }
}

#[test]
fn at_scope_matches_its_own_scope() {
    assert!(entry_at("user").at_scope(&Scope::User));
    assert!(entry_at("project").at_scope(&Scope::Project { path: PathBuf::from("/x") }));
}

#[test]
fn at_scope_refuses_the_other_scope() {
    assert!(!entry_at("project").at_scope(&Scope::User));
    assert!(!entry_at("user").at_scope(&Scope::Project { path: PathBuf::from("/x") }));
}

#[test]
fn at_scope_tolerates_a_missing_scope_field() {
    // Tolerant serde: an entry CC emitted without `scope` matches either lookup,
    // so a lookup never misreads it as absent.
    let mut e = entry_at("user");
    e.scope = None;
    assert!(e.at_scope(&Scope::User));
    assert!(e.at_scope(&Scope::Project { path: PathBuf::from("/x") }));
}
