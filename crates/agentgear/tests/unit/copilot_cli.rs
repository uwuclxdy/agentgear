//! copilot-cli native-backend unit tests: the TEXT parsers for copilot's `plugin
//! list` / `plugin marketplace list` (copilot has no `--json`), the version parse
//! that tolerates copilot's `GitHub Copilot CLI 1.0.92.` shape, and the pure
//! present/classify decisions reconcile + probe key on. The real round-trip against
//! the `copilot` binary is the docker leg
//! (`crates/host-fixture/tests/docker/copilot-cli`), which cannot run under
//! `cargo test`, so nothing here spawns `copilot`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::{
    BackendState, EnabledPluginsKey, PresentAction, StaleKey, classify, enabled_plugins_key_from, enabled_plugins_rm_key,
    github_marketplace_source, install_is_frozen, local_marketplace_is_ours, marketplace_foreign_reason, marketplace_siblings,
    present_action, shared_sibling_clause, stale_key_note, stale_key_plan,
};
use crate::cli::{
    CopilotMarketplace, CopilotPlugin, Output, copilot_meets_floor, parse_marketplace_list, parse_plugin_list, parse_version_anywhere,
};
use crate::error::Error;
use crate::host::{Plugin, Source};

fn plugin(name: &str, mkt: &str, version: Option<&str>) -> CopilotPlugin {
    CopilotPlugin { plugin: name.into(), marketplace: mkt.into(), version: version.map(str::to_string), disabled: false }
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
fn parse_plugin_list_reads_the_disabled_suffix() {
    let out = "Installed plugins:\n  • a@one (v1.2.3) (disabled)\n";
    assert_eq!(
        parse_plugin_list(out),
        vec![CopilotPlugin { plugin: "a".into(), marketplace: "one".into(), version: Some("1.2.3".into()), disabled: true }]
    );
    let no_version = "Installed plugins:\n  • a@one (disabled)\n";
    assert_eq!(
        parse_plugin_list(no_version),
        vec![CopilotPlugin { plugin: "a".into(), marketplace: "one".into(), version: None, disabled: true }]
    );
    let enabled = "Installed plugins:\n  • a@one (v0.1.0) (enabled)\n";
    assert_eq!(parse_plugin_list(enabled), vec![plugin("a", "one", Some("0.1.0"))]);
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
        vec![CopilotMarketplace { name: "ez-fixture".into(), path: Some("/abs/path/current".into()), github: None }]
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
    assert_eq!(
        parse_marketplace_list(out),
        vec![CopilotMarketplace { name: "mine".into(), path: None, github: Some("owner/repo".into()) }]
    );
}

#[test]
fn parse_marketplace_list_reads_a_url_row_as_setting_neither_field() {
    let out = "Registered marketplaces:\n  • mine (URL: https://github.com/owner/repo)\n";
    assert_eq!(parse_marketplace_list(out), vec![CopilotMarketplace { name: "mine".into(), path: None, github: None }]);
}

// --- version parse (copilot puts the version last) ---------------------------

#[test]
fn parse_version_anywhere_finds_the_trailing_version() {
    assert_eq!(parse_version_anywhere("GitHub Copilot CLI 1.0.92."), Some((1, 0, 92)));
    assert_eq!(parse_version_anywhere("nope"), None);
}

#[test]
fn copilot_floor_is_pinned_at_1_0_92() {
    assert_eq!(copilot_meets_floor("GitHub Copilot CLI 1.0.91."), Some(false));
    assert_eq!(copilot_meets_floor("GitHub Copilot CLI 1.0.92."), Some(true));
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
    assert_eq!(present_action(&src, Some("0.1.0"), "0.2.0", true), PresentAction::Update);
    assert_eq!(present_action(&src, Some("0.2.0"), "0.2.0", true), PresentAction::NoOp);
    // unparseable / missing installed => never churn, and never freeze either: neither
    // older nor newer, so it stays converged.
    assert_eq!(present_action(&src, None, "0.2.0", true), PresentAction::NoOp);
    assert_eq!(present_action(&src, Some("weird"), "0.2.0", true), PresentAction::NoOp);
}

#[test]
fn present_action_freezes_a_strictly_newer_install() {
    // A strictly-newer install belongs to a coexisting newer binary: never downgraded
    // (that was always true) and now `Frozen` rather than `NoOp`, because a newer
    // binary's tree is not this older one's to replace on any pass.
    let src = Source::Embedded;
    assert_eq!(present_action(&src, Some("0.3.0"), "0.2.0", true), PresentAction::Frozen);
    assert_eq!(present_action(&src, Some("1.0.0"), "0.2.0", true), PresentAction::Frozen);
    // Same for a `--path` install: the freeze is about who owns the install, not the
    // source it came from.
    assert_eq!(present_action(&Source::Path(PathBuf::from("/tmp/tree")), Some("0.3.0"), "0.2.0", true), PresentAction::Frozen);
}

#[test]
fn present_action_refreshes_a_same_version_tree_change_but_never_outranks_the_others() {
    // The staleness class: a same-version tree edit leaves the registry's version
    // column unchanged, so it is invisible to every version comparison here. It has
    // to be its own term, and the last one.
    let src = Source::Embedded;
    assert_eq!(present_action(&src, Some("0.2.0"), "0.2.0", false), PresentAction::Refresh);
    assert_eq!(present_action(&src, None, "0.2.0", false), PresentAction::Refresh);
    // A version bump converges through re-staging the pointer: `Update`.
    assert_eq!(present_action(&src, Some("0.1.0"), "0.2.0", false), PresentAction::Update);
    // A newer binary's install is not ours to replace, whatever tree we hold.
    assert_eq!(present_action(&src, Some("0.3.0"), "0.2.0", false), PresentAction::Frozen);
    // github has no local tree to compare, so it can never reach this term.
    assert_eq!(present_action(&github(), Some("0.2.0"), "0.2.0", false), PresentAction::NoOp);
}

#[test]
fn present_action_github_present_is_converged_no_version_churn() {
    // copilot can't pin a ref, so the default-branch version is unrelated to the
    // baked one — a present github install must NoOp, never re-staged each session
    // even when the versions differ in either direction.
    assert_eq!(present_action(&github(), Some("0.1.0"), "0.2.0", true), PresentAction::NoOp);
    assert_eq!(present_action(&github(), Some("0.9.0"), "0.2.0", true), PresentAction::NoOp);
    assert_eq!(present_action(&github(), None, "0.2.0", true), PresentAction::NoOp);
}

#[test]
fn present_action_github_outranks_the_freeze() {
    // The github arm is matched FIRST, so a newer-looking github install is `NoOp`
    // (converged), not `Frozen`. It has to be: its version tracks a default branch,
    // so a newer number there is drift, not another binary's install — freezing on it
    // would silently stop converging a github host whose default branch moved ahead of
    // the baked version.
    assert_eq!(present_action(&github(), Some("9.9.9"), "0.2.0", true), PresentAction::NoOp);
}

#[test]
fn install_is_frozen_never_applies_to_github() {
    // `present_action` matches the github arm first, so a github install is never
    // Frozen however its tracked version compares — and the probe must keep folding the
    // guidance block in for it, because reconcile does write that block.
    assert!(!install_is_frozen(&github(), Some(&plugin("p", "m", Some("9.9.9"))), "0.2.0"), "a github install never freezes");
    assert!(
        install_is_frozen(&Source::Embedded, Some(&plugin("p", "m", Some("0.3.0"))), "0.2.0"),
        "a strictly-newer local install freezes"
    );
    assert!(!install_is_frozen(&Source::Embedded, Some(&plugin("p", "m", Some("0.2.0"))), "0.2.0"), "an equal version is ours to manage");
    assert!(
        !install_is_frozen(&Source::Embedded, Some(&plugin("p", "m", None)), "0.2.0"),
        "an unparsed version is not evidence of a newer install"
    );
}

#[test]
fn classify_maps_presence_and_version_to_state() {
    let src = Source::Embedded;
    assert!(matches!(classify(&src, None, "0.2.0", true), BackendState::Absent));
    assert!(matches!(classify(&src, Some(&plugin("p", "m", Some("0.1.0"))), "0.2.0", true), BackendState::NeedsRepair));
    assert!(matches!(classify(&src, Some(&plugin("p", "m", Some("0.2.0"))), "0.2.0", true), BackendState::Healthy));
    // a strictly-newer install reads Healthy (monotonic — never repair/downgrade it).
    assert!(matches!(classify(&src, Some(&plugin("p", "m", Some("0.9.0"))), "0.2.0", true), BackendState::Healthy));
}

#[test]
fn classify_reads_a_disabled_row_as_disabled_and_outranking_drift() {
    let src = Source::Embedded;
    let disabled = CopilotPlugin { plugin: "p".into(), marketplace: "m".into(), version: Some("0.2.0".into()), disabled: true };
    assert!(matches!(classify(&src, Some(&disabled), "0.2.0", true), BackendState::Disabled));
    // A deliberate disable outranks drift and version, exactly like the other
    // disable-capable backends: self_heal must never repair or re-enable it.
    assert!(matches!(classify(&src, Some(&disabled), "0.3.0", false), BackendState::Disabled));
    assert!(matches!(classify(&github(), Some(&disabled), "0.2.0", true), BackendState::Disabled));
}

#[test]
fn classify_reads_a_same_version_tree_change_as_needing_repair() {
    // self_heal's `(marker present, Healthy)` row mutates nothing, so a drifted tree
    // that classifies Healthy here converges only on an explicit `setup`. The version
    // terms cannot see it: a same-version edit leaves the version column unchanged.
    let src = Source::Embedded;
    assert!(matches!(classify(&src, Some(&plugin("p", "m", Some("0.2.0"))), "0.2.0", false), BackendState::NeedsRepair));
    // Monotonic still outranks it: a strictly-newer install belongs to another binary.
    assert!(matches!(classify(&src, Some(&plugin("p", "m", Some("0.9.0"))), "0.2.0", false), BackendState::Healthy));
    // github has no local tree to compare, so it can never reach this term.
    assert!(matches!(classify(&github(), Some(&plugin("p", "m", Some("0.2.0"))), "0.2.0", false), BackendState::Healthy));
    // Absent stays absent: self_heal must never resurrect a deliberate uninstall.
    assert!(matches!(classify(&src, None, "0.2.0", false), BackendState::Absent));
}

#[test]
fn classify_github_present_is_healthy_regardless_of_version() {
    assert!(matches!(classify(&github(), None, "0.2.0", true), BackendState::Absent));
    // a stale-LOOKING version must NOT read NeedsRepair for github (would churn on a
    // default-branch that simply differs from the baked version).
    assert!(matches!(classify(&github(), Some(&plugin("p", "m", Some("0.1.0"))), "0.2.0", true), BackendState::Healthy));
}

// --- remove: ownership + refcount + dotted-key guards ------------------------

fn test_plugin(name: &'static str, marketplace: &'static str) -> Plugin {
    Plugin { name, marketplace, version: "0.1.0", agents: &[], instructions: None, blob: &[] }
}

#[test]
fn local_marketplace_is_ours_matches_the_pointer_literal_and_legacy_current() {
    let ours = Path::new("/nonexistent-ag/ez-fixture-plugin/current@copilot-cli");
    assert!(local_marketplace_is_ours("/nonexistent-ag/ez-fixture-plugin/current@copilot-cli", ours));
    // The pre-scoping layout registered a plain `current` under the same data root.
    assert!(local_marketplace_is_ours("/nonexistent-ag/ez-fixture-plugin/current", ours));
    assert!(!local_marketplace_is_ours("/elsewhere", ours), "a same-named marketplace elsewhere is foreign");
}

/// A symlink alias to our pointer must count as ours: the registered path resolves
/// to the same realpath, so a literal-only compare would fail a real alias.
#[cfg(unix)]
#[test]
fn local_marketplace_is_ours_matches_a_symlink_alias_through_canonicalize() {
    let root = crate::scratch::path("agentgear-copilot-alias");
    let ours = root.join("current@copilot-cli");
    std::fs::create_dir_all(&ours).unwrap();
    let alias = root.join("alias");
    std::os::unix::fs::symlink(&ours, &alias).unwrap();

    assert!(local_marketplace_is_ours(alias.to_str().unwrap(), &ours));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn marketplace_foreign_reason_github_requires_the_source_repo() {
    let plugin = test_plugin("mine", "mine");
    let source = Source::GitHub { repo: "owner/repo", ref_: "v0.1.0" };
    let entry = |github: Option<&str>| CopilotMarketplace { name: "mine".into(), path: None, github: github.map(str::to_string) };
    assert_eq!(marketplace_foreign_reason(&entry(Some("owner/repo")), &plugin, &source).unwrap(), None);
    assert!(
        marketplace_foreign_reason(&entry(Some("someone/else")), &plugin, &source).unwrap().is_some(),
        "a same-named github marketplace from another repo is foreign"
    );
    assert!(marketplace_foreign_reason(&entry(None), &plugin, &source).unwrap().is_some());
}

#[test]
fn marketplace_siblings_names_only_installed_plugins_from_the_marketplace() {
    let host_plugin = test_plugin("ez-fixture-plugin", "ez-fixture-plugin");
    let ours = plugin("ez-fixture-plugin", "ez-fixture-plugin", None);
    let sibling = plugin("other-plugin", "ez-fixture-plugin", None);
    let elsewhere = plugin("other-plugin", "their-marketplace", None);
    let installed = HashSet::from(["other-plugin@ez-fixture-plugin".to_string()]);
    let none = HashSet::new();
    assert!(marketplace_siblings(std::slice::from_ref(&ours), &host_plugin, &none).is_empty(), "our own row is not a sibling");
    assert_eq!(
        marketplace_siblings(&[ours.clone(), sibling.clone()], &host_plugin, &installed),
        vec!["other-plugin@ez-fixture-plugin".to_string()]
    );
    assert!(marketplace_siblings(&[elsewhere], &host_plugin, &installed).is_empty(), "a plugin from another marketplace is not a sibling");
    // A merely-listed sibling (no `enabledPlugins` key) is not an install.
    assert!(
        marketplace_siblings(&[ours.clone(), sibling], &host_plugin, &none).is_empty(),
        "a keyless listing is not an installed sibling"
    );
}

#[test]
fn shared_sibling_clause_is_plural_correct() {
    // One sibling uses the singular verb; several use the plural and join the names.
    assert_eq!(shared_sibling_clause(&["a".to_string()]), "`a` still uses it");
    assert_eq!(shared_sibling_clause(&["a".to_string(), "b".to_string()]), "`a`, `b` still use it");
}

#[test]
fn enabled_plugins_rm_key_is_dotted_only_when_the_id_has_a_dot() {
    let plain = test_plugin("ez-fixture-plugin", "ez-fixture-plugin");
    assert_eq!(enabled_plugins_rm_key(&plain), ("enabledPlugins.ez-fixture-plugin@ez-fixture-plugin".to_string(), false));
    let dotted = test_plugin("my.plugin", "my.marketplace");
    assert_eq!(enabled_plugins_rm_key(&dotted), ("enabledPlugins.my.plugin@my.marketplace".to_string(), true));
}

#[test]
fn stale_key_plan_names_a_dotted_id_but_clears_a_plain_one() {
    let plain = test_plugin("ez-fixture-plugin", "ez-fixture-plugin");
    assert_eq!(stale_key_plan(&plain, true), StaleKey::Clear);
    let dotted = test_plugin("my.plugin", "my.marketplace");
    assert_eq!(stale_key_plan(&dotted, true), StaleKey::Name);
    assert_eq!(stale_key_plan(&dotted, false), StaleKey::Absent);
}

#[test]
fn stale_key_note_names_a_dotted_leftover_by_equality() {
    let dotted = test_plugin("my.plugin", "my.marketplace");
    assert_eq!(
        stale_key_note(&dotted),
        "agentgear: `enabledPlugins.my.plugin@my.marketplace` still names our id and `copilot config --rm` cannot clear a dotted key; remove it by hand if it is yours"
    );
}

#[test]
fn marketplace_foreign_reason_url_form_is_not_ours() {
    let plugin = test_plugin("mine", "mine");
    let entry = CopilotMarketplace { name: "mine".into(), path: None, github: None };
    let source = Source::GitHub { repo: "owner/repo", ref_: "v0.1.0" };
    assert!(marketplace_foreign_reason(&entry, &plugin, &source).unwrap().is_some());
}

#[test]
fn enabled_plugins_key_from_fails_loud_on_nonzero_with_output() {
    let plugin = test_plugin("ez-fixture-plugin", "ez-fixture-plugin");
    let out = Output { code: 1, stdout: vec![], stderr: b"boom".to_vec() };
    let err = enabled_plugins_key_from(&out, &plugin).unwrap_err();
    assert!(matches!(err, Error::Cli { .. }), "a nonzero read with output must be a CLI error, got {err:?}");
}

#[test]
fn enabled_plugins_key_from_carries_stdout_when_stderr_is_empty() {
    let plugin = test_plugin("ez-fixture-plugin", "ez-fixture-plugin");
    let out = Output { code: 1, stdout: b"config: unknown key".to_vec(), stderr: vec![] };
    let err = enabled_plugins_key_from(&out, &plugin).unwrap_err();
    match err {
        Error::Cli { stderr, .. } => {
            assert_eq!(stderr, "config: unknown key", "the stdout-only detail must ride in stderr");
        }
        other => panic!("expected a CLI error carrying stdout, got {other:?}"),
    }
}

#[test]
fn enabled_plugins_key_from_fails_loud_on_unparseable_stdout() {
    let plugin = test_plugin("ez-fixture-plugin", "ez-fixture-plugin");
    let out = Output { code: 0, stdout: b"not json".to_vec(), stderr: vec![] };
    let err = enabled_plugins_key_from(&out, &plugin).unwrap_err();
    assert!(matches!(err, Error::Json { .. }), "an unparseable read must be a JSON error, got {err:?}");
}

#[test]
fn enabled_plugins_key_from_reads_absent_false_and_true() {
    let plugin = test_plugin("ez-fixture-plugin", "ez-fixture-plugin");
    let absent = Output { code: 1, stdout: vec![], stderr: vec![] };
    assert_eq!(enabled_plugins_key_from(&absent, &plugin).unwrap(), EnabledPluginsKey::Absent);
    let disabled =
        Output { code: 0, stdout: br#"{"ez-fixture-plugin@ez-fixture-plugin": false}"#.to_vec(), stderr: b"settings.json".to_vec() };
    assert_eq!(enabled_plugins_key_from(&disabled, &plugin).unwrap(), EnabledPluginsKey::Disabled);
    let enabled =
        Output { code: 0, stdout: br#"{"ez-fixture-plugin@ez-fixture-plugin": true}"#.to_vec(), stderr: b"settings.json".to_vec() };
    assert_eq!(enabled_plugins_key_from(&enabled, &plugin).unwrap(), EnabledPluginsKey::Enabled);
    let other = test_plugin("other", "m");
    assert_eq!(enabled_plugins_key_from(&disabled, &other).unwrap(), EnabledPluginsKey::Absent);
}
