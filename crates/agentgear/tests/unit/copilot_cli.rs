//! copilot-cli native-backend unit tests: the TEXT parsers for copilot's `plugin
//! list` / `plugin marketplace list` (copilot has no `--json`), the version parse
//! that tolerates copilot's `GitHub Copilot CLI 1.0.71.` shape, and the pure
//! present/classify decisions reconcile + probe key on. The real round-trip against
//! the `copilot` binary is the docker leg
//! (`crates/host-fixture/tests/docker/copilot-cli`), which cannot run under
//! `cargo test`, so nothing here spawns `copilot`.

use std::path::PathBuf;

use super::{BackendState, PresentAction, classify, github_marketplace_source, present_action};
use crate::cli::{CopilotMarketplace, CopilotPlugin, parse_marketplace_list, parse_plugin_list, parse_version_anywhere};
use crate::host::Source;

fn plugin(name: &str, mkt: &str, version: Option<&str>) -> CopilotPlugin {
    CopilotPlugin { plugin: name.into(), marketplace: mkt.into(), version: version.map(str::to_string) }
}

fn github() -> Source {
    Source::GitHub { repo: "owner/repo", ref_: "v0.1.0" }
}

// --- plugin list parser ------------------------------------------------------

#[test]
fn parse_plugin_list_reads_id_and_version() {
    let out = "Installed plugins:\n  • ez-fixture-plugin@ez-fixture (v0.1.0)\n";
    assert_eq!(parse_plugin_list(out), vec![plugin("ez-fixture-plugin", "ez-fixture", Some("0.1.0"))]);
}

#[test]
fn parse_plugin_list_handles_multiple_rows_and_skips_the_header() {
    let out = "Installed plugins:\n  • a@one (v1.2.3)\n  • b@two (v0.0.9)\n";
    assert_eq!(parse_plugin_list(out), vec![plugin("a", "one", Some("1.2.3")), plugin("b", "two", Some("0.0.9"))]);
}

#[test]
fn parse_plugin_list_empty_when_none_installed() {
    assert!(parse_plugin_list("No plugins installed.\n").is_empty());
    assert!(parse_plugin_list("").is_empty());
}

// --- marketplace list parser -------------------------------------------------

const MARKETPLACE_OUT: &str = "\
Included with GitHub Copilot:
  ◆ copilot-plugins (GitHub: github/copilot-plugins)
  ◆ awesome-copilot (GitHub: github/awesome-copilot)

Registered marketplaces:
  • ez-fixture (Local: /abs/path/current)
";

#[test]
fn parse_marketplace_list_returns_only_registered_local() {
    // The built-in `Included with GitHub Copilot:` rows are never ours; only the
    // `Registered marketplaces:` section, with the local path extracted.
    assert_eq!(
        parse_marketplace_list(MARKETPLACE_OUT),
        vec![CopilotMarketplace { name: "ez-fixture".into(), path: Some("/abs/path/current".into()) }]
    );
}

#[test]
fn parse_marketplace_list_skips_builtin_only_output() {
    let only_builtin = "Included with GitHub Copilot:\n  ◆ copilot-plugins (GitHub: github/copilot-plugins)\n";
    assert!(parse_marketplace_list(only_builtin).is_empty(), "built-in marketplaces must never count as ours");
}

#[test]
fn parse_marketplace_list_reads_a_registered_github_source() {
    let out = "Registered marketplaces:\n  • mine (GitHub: owner/repo)\n";
    assert_eq!(parse_marketplace_list(out), vec![CopilotMarketplace { name: "mine".into(), path: None }]);
}

// --- version parse (copilot puts the version last) ---------------------------

#[test]
fn parse_version_anywhere_finds_the_trailing_version() {
    assert_eq!(parse_version_anywhere("GitHub Copilot CLI 1.0.71."), Some((1, 0, 71)));
    assert_eq!(parse_version_anywhere("nope"), None);
}

// --- github marketplace source (unpinnable ref) ------------------------------

#[test]
fn github_marketplace_source_drops_the_unpinnable_ref() {
    // `owner/repo@ref` breaks copilot (parsed as a marketplace name; `.git` appended
    // on clone), so only the bare repo is sent — copilot tracks the default branch.
    assert_eq!(github_marketplace_source("owner/repo", "v0.1.0"), "owner/repo");
    assert_eq!(github_marketplace_source("owner/repo", ""), "owner/repo");
}

// --- reconcile / probe decisions ---------------------------------------------

#[test]
fn present_action_updates_only_a_stale_non_github_install() {
    let src = Source::Embedded;
    assert_eq!(present_action(&src, Some("0.1.0"), "0.2.0"), PresentAction::Update);
    assert_eq!(present_action(&src, Some("0.2.0"), "0.2.0"), PresentAction::NoOp);
    // unparseable / missing installed => never churn, and never freeze either: neither
    // older nor newer, so the slot still converges.
    assert_eq!(present_action(&src, None, "0.2.0"), PresentAction::NoOp);
    assert_eq!(present_action(&src, Some("weird"), "0.2.0"), PresentAction::NoOp);
}

#[test]
fn present_action_freezes_a_strictly_newer_install() {
    // A strictly-newer install belongs to a coexisting newer binary: never downgraded
    // (that was always true) and now `Frozen` rather than `NoOp`, because the two
    // differ on the status-line slot. `NoOp` converges the slot; taking it here would
    // point a newer binary's status line at this older one, on every session.
    let src = Source::Embedded;
    assert_eq!(present_action(&src, Some("0.3.0"), "0.2.0"), PresentAction::Frozen);
    assert_eq!(present_action(&src, Some("1.0.0"), "0.2.0"), PresentAction::Frozen);
    // Same for a `--path` install: the freeze is about who owns the install, not the
    // source it came from.
    assert_eq!(present_action(&Source::Path(PathBuf::from("/tmp/tree")), Some("0.3.0"), "0.2.0"), PresentAction::Frozen);
}

#[test]
fn present_action_github_present_is_converged_no_version_churn() {
    // copilot can't pin a ref, so the default-branch version is unrelated to the
    // baked one — a present github install must NoOp, never `plugin update` each
    // session even when the versions differ in either direction.
    assert_eq!(present_action(&github(), Some("0.1.0"), "0.2.0"), PresentAction::NoOp);
    assert_eq!(present_action(&github(), Some("0.9.0"), "0.2.0"), PresentAction::NoOp);
    assert_eq!(present_action(&github(), None, "0.2.0"), PresentAction::NoOp);
}

#[test]
fn present_action_github_outranks_the_freeze() {
    // The github arm is matched FIRST, so a newer-looking github install is `NoOp`
    // (converged, slot written), not `Frozen`. It has to be: its version tracks a
    // default branch, so a newer number there is drift, not another binary's install —
    // freezing on it would silently stop writing the slot for every github host whose
    // default branch moved ahead of the baked version.
    assert_eq!(present_action(&github(), Some("9.9.9"), "0.2.0"), PresentAction::NoOp);
}

#[test]
fn classify_maps_presence_and_version_to_state() {
    let src = Source::Embedded;
    assert!(matches!(classify(&src, None, "0.2.0"), BackendState::Absent));
    assert!(matches!(classify(&src, Some(&plugin("p", "m", Some("0.1.0"))), "0.2.0"), BackendState::NeedsRepair));
    assert!(matches!(classify(&src, Some(&plugin("p", "m", Some("0.2.0"))), "0.2.0"), BackendState::Healthy));
    // a strictly-newer install reads Healthy (monotonic — never repair/downgrade it).
    assert!(matches!(classify(&src, Some(&plugin("p", "m", Some("0.9.0"))), "0.2.0"), BackendState::Healthy));
}

#[test]
fn classify_github_present_is_healthy_regardless_of_version() {
    assert!(matches!(classify(&github(), None, "0.2.0"), BackendState::Absent));
    // a stale-LOOKING version must NOT read NeedsRepair for github (would churn on a
    // default-branch that simply differs from the baked version).
    assert!(matches!(classify(&github(), Some(&plugin("p", "m", Some("0.1.0"))), "0.2.0"), BackendState::Healthy));
}
