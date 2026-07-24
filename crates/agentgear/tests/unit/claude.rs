//! claude backend pure decisions, in two groups.
//!
//! Ref-pinning: the github source string (`owner/repo@ref`) and the present-branch
//! re-point rule (matching ref → update, drifted → re-add). See docs/design.md
//! §ref-pinning ground truth.
//!
//! statusLine: what the slot write renders, and whose value is already in the slot.
//!
//! No CLI/fs in either — the mutating calls are IO-only; these guard the decisions
//! that pick what they send.

use std::path::PathBuf;

use super::{MarketplaceHealth, MarketplaceOp, github_source, is_ours, marketplace_health, marketplace_op, rendered_statusline};
use crate::host::{Plugin, Source};
use crate::manifest::MarketplaceEntry;
use crate::statusline::StatusLineDecl;

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

// statusLine: the two pure decisions behind the slot write — what we render, and
// whose value is already sitting there.

fn plugin_with(statusline: Option<StatusLineDecl>) -> Plugin {
    Plugin { name: "ez-sl", marketplace: "ez-mkt", version: "0.1.0", agents: &["claude"], instructions: None, statusline, blob: &[] }
}

#[test]
fn rendered_statusline_expands_the_client_token() {
    let plugin = plugin_with(Some(StatusLineDecl::new("mytool statusline --client ${AGENTGEAR_CLIENT}").with_padding(0)));
    let (value, command) = rendered_statusline(&plugin).expect("a declared status line must render");
    assert_eq!(command, "mytool statusline --client claude");
    assert_eq!(value, serde_json::json!({"type": "command", "command": "mytool statusline --client claude", "padding": 0}));
}

#[test]
fn rendered_statusline_is_none_without_a_declaration() {
    assert!(rendered_statusline(&plugin_with(None)).is_none());
}

#[test]
fn rendered_statusline_is_none_for_a_blank_command() {
    // `StatusLineDecl::default()` carries one. Rendering it would displace (and
    // stash) the user's real status line in exchange for a command that does
    // nothing, so a host bug here must cost them nothing.
    assert!(rendered_statusline(&plugin_with(Some(StatusLineDecl::default()))).is_none());
    assert!(rendered_statusline(&plugin_with(Some(StatusLineDecl::new("   ").with_padding(0)))).is_none());
}

#[test]
fn is_ours_matches_on_the_command_not_the_whole_object() {
    let ours = "mytool statusline --client claude";
    // Our own earlier rendering, padding since changed by a host release: still ours,
    // so it is never stashed as the user's original.
    assert!(is_ours(&serde_json::json!({"type": "command", "command": ours, "padding": 1}), ours));
    assert!(is_ours(&serde_json::json!({"type": "command", "command": ours}), ours));
    // Genuinely someone else's, and shapes with no command at all.
    assert!(!is_ours(&serde_json::json!({"type": "command", "command": "their-bar", "padding": 0}), ours));
    assert!(!is_ours(&serde_json::json!({"type": "command"}), ours));
    assert!(!is_ours(&serde_json::json!("their-bar"), ours));
}
