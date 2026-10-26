//! The claude backend's spawn-free heal-needed classifier: file fixtures pin the
//! verdict against self_heal's state table + probe + reconcile reach (design
//! §self_heal), using the 2.1.310 registry ground truth. Every fixture seeds CC's
//! registry files under a scratch config dir and a scratch data root; nothing
//! touches the real HOME or a real `claude`.
//!
//! The classifier under test is `heal_needed_from`: the caller (this file) hands
//! it the already-resolved source, marker, config dir, data root, restart-pending
//! flag, whether the firing session event could retire it, and cwd. The
//! `CLAUDE_CONFIG_DIR`/home resolution behind the config dir is unit-tested in
//! `ccregistry_claude.rs`, not here.

use std::path::{Path, PathBuf};

use crate::host::{HealNeededVerdict, Plugin, Source};
use crate::materialize::{TreeSource, content_hash};
use crate::stamp::Marker;

const NAME: &str = "ez-heal";
const MKT: &str = "ez-heal-mkt";
const ID: &str = "ez-heal@ez-heal-mkt";
const CLIENT: &str = "claude";

fn plugin() -> Plugin {
    Plugin { name: NAME, marketplace: MKT, version: "0.1.0", agents: &["claude"], instructions: None, blob: &[] }
}

fn marker(source_mode: &str, tree_hash: Option<String>, reinstalling: bool) -> Marker {
    Marker {
        binary_version: "0.1.0".into(),
        plugin_version: "0.1.0".into(),
        source_mode: source_mode.into(),
        scope: "user".into(),
        agent: "claude".into(),
        project_path: None,
        source_path: None,
        statusline_original: None,
        tree_hash,
        reinstalling,
    }
}

/// One scratch root holding the CC config dir (registry files), the agentgear
/// data root (the `current@claude` pointer path), the path-source tree, a process
/// cwd (for the cwd-merged enable bit), and an existing dir to point `installPath` at.
struct Fixture {
    root: PathBuf,
    config: PathBuf,
    data: PathBuf,
    cwd: PathBuf,
    tree: PathBuf,
    install: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = crate::scratch::path("ez-heal-needed");
        let _ = std::fs::remove_dir_all(&root);
        let config = root.join("cfg");
        let data = root.join("data");
        let cwd = root.join("cwd");
        let tree = root.join("tree");
        let install = root.join("install");
        for dir in [&config, &data, &cwd, &tree, &install] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(config.join("plugins")).unwrap();
        std::fs::create_dir_all(tree.join("commands")).unwrap();
        std::fs::write(tree.join("commands").join("hello.md"), "# hi\n").unwrap();
        // `content_hash` refuses a dir without a plugin manifest, so the path
        // source has to look like a real plugin tree.
        std::fs::create_dir_all(tree.join(".claude-plugin")).unwrap();
        std::fs::write(tree.join(".claude-plugin").join("plugin.json"), br#"{"name":"ez-heal","version":"0.1.0"}"#).unwrap();
        Fixture { root, config, data, cwd, tree, install }
    }

    /// The materialized pointer the classifier compares the registered path to.
    fn pointer(&self) -> PathBuf {
        self.data.join(format!("current@{CLIENT}"))
    }

    /// The hash the classifier computes for the path source.
    fn staged_hash(&self) -> String {
        content_hash(TreeSource::Dir(&self.tree), CLIENT).unwrap()
    }

    fn path_source(&self) -> Source {
        Source::Path(self.tree.clone())
    }

    /// Write `installed_plugins.json` with one row for `ID` at `scope`.
    fn seed_installed(&self, version: Option<&str>, install_path: Option<&Path>, scope: &str, project_path: Option<&Path>) {
        let mut row = serde_json::Map::new();
        row.insert("scope".into(), serde_json::json!(scope));
        if let Some(version) = version {
            row.insert("version".into(), serde_json::json!(version));
        }
        if let Some(path) = install_path {
            row.insert("installPath".into(), serde_json::json!(path.to_string_lossy()));
        }
        if let Some(project) = project_path {
            row.insert("projectPath".into(), serde_json::json!(project.to_string_lossy()));
        }
        let doc = serde_json::json!({ "version": 2, "plugins": { (ID): [serde_json::Value::Object(row)] } });
        self.write_registry("installed_plugins.json", &doc);
    }

    /// Write `known_marketplaces.json` with a `directory` entry whose registered
    /// path and `installLocation` are both `path`.
    fn seed_marketplace(&self, path: Option<&Path>, source: &str) {
        let mut source_obj = serde_json::Map::new();
        source_obj.insert("source".into(), serde_json::json!(source));
        if let Some(path) = path {
            source_obj.insert("path".into(), serde_json::json!(path.to_string_lossy()));
        }
        let doc = serde_json::json!({
            (MKT): {
                "source": serde_json::Value::Object(source_obj),
                "installLocation": path.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                "lastUpdated": "2026-10-25T00:00:00.000Z",
            }
        });
        self.write_registry("known_marketplaces.json", &doc);
    }

    /// Write `known_marketplaces.json` with a `github` entry whose `installLocation`
    /// (its clone dir) is `install_location`; github entries carry no `path`.
    fn seed_github_marketplace_at(&self, install_location: &Path) {
        let doc = serde_json::json!({
            (MKT): {
                "source": { "source": "github" },
                "installLocation": install_location.to_string_lossy().into_owned(),
                "lastUpdated": "2026-10-25T00:00:00.000Z",
            }
        });
        self.write_registry("known_marketplaces.json", &doc);
    }

    /// Write the user-scope `<cfg>/settings.json` enable bit; `None` writes no
    /// `enabledPlugins` key (absent = disabled).
    fn seed_settings(&self, enabled: Option<bool>) {
        let doc = match enabled {
            Some(value) => serde_json::json!({ "enabledPlugins": { (ID): value } }),
            None => serde_json::json!({}),
        };
        std::fs::write(self.config.join("settings.json"), serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    }

    /// Write one cwd-layer settings file (`settings.local.json` or `settings.json`)
    /// under `<cwd>/.claude/` with the same enable-bit shape.
    fn seed_cwd_settings(&self, file: &str, enabled: Option<bool>) {
        let doc = match enabled {
            Some(value) => serde_json::json!({ "enabledPlugins": { (ID): value } }),
            None => serde_json::json!({}),
        };
        std::fs::create_dir_all(self.cwd.join(".claude")).unwrap();
        std::fs::write(self.cwd.join(".claude").join(file), serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    }

    /// Write the generated marketplace manifest at `dir`, the presence the
    /// health classifier checks.
    fn seed_manifest_at(&self, dir: &Path) {
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(dir.join(".claude-plugin").join("marketplace.json"), "{}").unwrap();
    }

    fn write_registry(&self, name: &str, doc: &serde_json::Value) {
        std::fs::write(self.config.join("plugins").join(name), serde_json::to_vec_pretty(doc).unwrap()).unwrap();
    }

    fn write_raw(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.config.join("plugins").join(name), bytes).unwrap();
    }

    fn verdict(&self, plugin: &Plugin, source: &Source, marker: Option<&Marker>) -> HealNeededVerdict {
        self.verdict_with_restart(plugin, source, marker, false)
    }

    fn verdict_with_restart(&self, plugin: &Plugin, source: &Source, marker: Option<&Marker>, restart_pending: bool) -> HealNeededVerdict {
        self.verdict_with_flags(plugin, source, marker, restart_pending, true)
    }

    /// The same verdict with the event's retire term spelled out: only a
    /// session event that loaded plugin contents (`startup` / `resume`) can
    /// retire a standing restart notice.
    fn verdict_with_flags(
        &self, plugin: &Plugin, source: &Source, marker: Option<&Marker>, restart_pending: bool, retire_notice: bool,
    ) -> HealNeededVerdict {
        let env =
            super::HealNeededEnv { config_dir: &self.config, data_root: &self.data, restart_pending, retire_notice, cwd: Some(&self.cwd) };
        super::heal_needed_from(plugin, source, marker, &env)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn no_registry_and_no_marker_is_clean() {
    let fx = Fixture::new();
    assert_eq!(fx.verdict(&plugin(), &fx.path_source(), None), HealNeededVerdict::Clean);
}

#[test]
fn no_row_at_scope_and_no_marker_is_clean() {
    // The registry lists the id at project scope only; a user-scope lookup must
    // not read it (the scope filter `find_plugin` keys on).
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "project", Some(&fx.root));
    assert_eq!(fx.verdict(&plugin(), &fx.path_source(), None), HealNeededVerdict::Clean);
}

#[test]
fn no_row_with_marker_is_needed() {
    let fx = Fixture::new();
    let m = marker("embedded", None, false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn no_row_with_reinstalling_marker_is_needed() {
    let fx = Fixture::new();
    let m = marker("embedded", None, true);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn healthy_marked_install_is_clean() {
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert_eq!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Clean);
}

#[test]
fn healthy_marked_with_restart_pending_is_needed() {
    // The healthy row clears the restart flag, so a pending flag is a change
    // self_heal would make -> Needed, never Clean.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert_eq!(
        fx.verdict_with_restart(&plugin(), &fx.path_source(), Some(&m), true),
        HealNeededVerdict::Needed { reason: "restart notice to retire".into() }
    );
}

#[test]
fn healthy_marked_with_a_standing_notice_under_a_midsession_event_is_clean() {
    // `/compact` and `/clear` fire SessionStart without reloading plugin contents,
    // so under them the healthy row cannot retire the notice: that clear is not a
    // change self_heal would make, and the pre-check must not send the host on the
    // two-spawn heal. A real session start passes `retire_notice: true` and reads
    // Needed above.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert_eq!(fx.verdict_with_flags(&plugin(), &fx.path_source(), Some(&m), true, false), HealNeededVerdict::Clean);
}

#[test]
fn healthy_marked_with_reinstalling_is_needed() {
    // A lingering reinstall beside a healthy row is ended by self_heal now
    // (a marker write), so the classifier must not read it Clean.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some(fx.staged_hash()), true);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn healthy_but_disabled_is_clean() {
    // A drifted tree under a disable still no-ops: the disabled row is checked
    // before the tree term, so drift never re-enables a deliberate disable.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(false));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some("different-hash".into()), false);
    assert_eq!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Clean);
}

#[test]
fn disabled_with_restart_pending_is_clean() {
    // The disabled row leaves the restart flag alone, so even a pending flag is
    // no change -> Clean.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(false));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert_eq!(fx.verdict_with_restart(&plugin(), &fx.path_source(), Some(&m), true), HealNeededVerdict::Clean);
}

#[test]
fn healthy_with_absent_enable_key_is_clean() {
    // A removed key reads `enabled: false` in `plugin list --json` (probed
    // 2.1.310), so absent = disabled, and the disabled row no-ops even on drift.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(None);
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some("different-hash".into()), false);
    assert_eq!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Clean);
}

#[test]
fn disabled_install_with_no_marker_is_needed() {
    // A disabled install this binary never marked still writes a marker on heal
    // (the adopt row), so it is Needed.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(false));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), None), HealNeededVerdict::Needed { .. }));
}

#[test]
fn healthy_unmarked_install_is_needed() {
    // The hash lives in the marker, so an install this binary never made has an
    // unaccounted tree: self_heal re-hands it rather than adopting.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), None), HealNeededVerdict::Needed { .. }));
}

#[test]
fn healthy_but_tree_hash_differs_is_needed() {
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some("different-hash".into()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn older_version_is_needed() {
    let fx = Fixture::new();
    fx.seed_installed(Some("0.0.9"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn strictly_newer_install_owned_elsewhere_is_clean() {
    // A newer install registered at another binary's own pointer is that
    // binary's working install: the monotonic short-circuit leaves it alone.
    let fx = Fixture::new();
    let other = fx.root.join("other-binary");
    std::fs::create_dir_all(&other).unwrap();
    fx.seed_manifest_at(&other);
    fx.seed_installed(Some("0.2.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&other), "directory");
    let m = marker("embedded", None, false);
    assert_eq!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Clean);
}

#[test]
fn strictly_newer_owned_with_reinstalling_is_needed() {
    // The owned no-op still rebuilds the marker (ending a lingering reinstall),
    // so a reinstalling marker is a change -> Needed.
    let fx = Fixture::new();
    let other = fx.root.join("other-binary");
    std::fs::create_dir_all(&other).unwrap();
    fx.seed_manifest_at(&other);
    fx.seed_installed(Some("0.2.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&other), "directory");
    let m = marker("embedded", None, true);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn strictly_newer_owned_with_restart_pending_is_needed() {
    // The owned no-op clears the restart flag, so a pending flag is a change.
    let fx = Fixture::new();
    let other = fx.root.join("other-binary");
    std::fs::create_dir_all(&other).unwrap();
    fx.seed_manifest_at(&other);
    fx.seed_installed(Some("0.2.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&other), "directory");
    let m = marker("embedded", None, false);
    assert_eq!(
        fx.verdict_with_restart(&plugin(), &fx.path_source(), Some(&m), true),
        HealNeededVerdict::Needed { reason: "restart notice to retire".into() }
    );
}

#[test]
fn strictly_newer_install_without_marker_is_needed() {
    // A strictly-newer owned install with no marker is not left alone: self_heal
    // takes the `(false, NeedsRepair)` row and writes a marker, so it is Needed.
    let fx = Fixture::new();
    let other = fx.root.join("other-binary");
    std::fs::create_dir_all(&other).unwrap();
    fx.seed_manifest_at(&other);
    fx.seed_installed(Some("0.2.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&other), "directory");
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), None), HealNeededVerdict::Needed { .. }));
}

#[test]
fn strictly_newer_install_with_dead_path_is_needed() {
    let fx = Fixture::new();
    let dead = fx.root.join("gone");
    fx.seed_installed(Some("0.2.0"), Some(&dead), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", None, false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn missing_install_path_is_needed() {
    let fx = Fixture::new();
    let dead = fx.root.join("gone");
    fx.seed_installed(Some("0.1.0"), Some(&dead), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn absent_marketplace_is_needed() {
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn marketplace_at_wrong_path_is_needed() {
    let fx = Fixture::new();
    let wrong = fx.root.join("wrong");
    std::fs::create_dir_all(&wrong).unwrap();
    fx.seed_manifest_at(&wrong);
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&wrong), "directory");
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn marketplace_manifest_missing_is_needed() {
    // The exact deadlock the heal gate exists for: the registered path resolves
    // but its manifest was deleted, so CC serves 0 hooks and 0 MCP.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn directory_source_with_unparseable_manifest_is_needed() {
    // A present but unparseable manifest is a load failure the file reader must
    // not read as healthy; presence alone is not enough.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    std::fs::create_dir_all(fx.pointer().join(".claude-plugin")).unwrap();
    std::fs::write(fx.pointer().join(".claude-plugin").join("marketplace.json"), b"{not json").unwrap();
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn unparseable_installed_registry_is_needed() {
    let fx = Fixture::new();
    fx.write_raw("installed_plugins.json", b"{not json");
    let m = marker("embedded", None, false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn installed_registry_with_other_version_is_needed() {
    // A schema bump (version != 2) is "cannot tell", never a row to read as
    // healthy — an otherwise-healthy registry under version 3 must not read Clean.
    let fx = Fixture::new();
    fx.write_registry(
        "installed_plugins.json",
        &serde_json::json!({
            "version": 3,
            "plugins": { (ID): [{ "scope": "user", "version": "0.1.0", "installPath": fx.install.to_string_lossy() }] }
        }),
    );
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn installed_registry_with_non_array_entry_is_needed() {
    // The v1 shape: `plugins[id]` is an object, not an array. With no marker this
    // must read "cannot tell" (Needed), never the no-row Clean a v1 registry would
    // otherwise collapse to.
    let fx = Fixture::new();
    fx.write_registry("installed_plugins.json", &serde_json::json!({ "version": 2, "plugins": { (ID): { "scope": "user" } } }));
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), None), HealNeededVerdict::Needed { .. }));
}

#[test]
fn installed_registry_with_non_object_row_is_needed() {
    // An install row that is not an object is "cannot tell", never a row to read
    // as healthy.
    let fx = Fixture::new();
    fx.write_registry("installed_plugins.json", &serde_json::json!({ "version": 2, "plugins": { (ID): [42] } }));
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn unparseable_marketplace_registry_is_needed() {
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.write_raw("known_marketplaces.json", b"{broken");
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn marketplace_registry_with_non_object_entry_is_needed() {
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.write_registry("known_marketplaces.json", &serde_json::json!({ (MKT): "not-an-object" }));
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn github_source_skips_the_tree_term() {
    // A github source has no local tree to hash; the rest of the terms apply,
    // including the installLocation manifest check.
    let fx = Fixture::new();
    let clone = fx.root.join("github-clone");
    fx.seed_manifest_at(&clone);
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_github_marketplace_at(&clone);
    let github = Source::GitHub { repo: "owner/repo", ref_: "v0.1.0" };
    let m = marker("github", None, false);
    assert_eq!(fx.verdict(&plugin(), &github, Some(&m)), HealNeededVerdict::Clean);
}

#[test]
fn github_source_with_missing_manifest_is_needed() {
    // A github clone whose manifest vanished is the load failure `probe` reads
    // off `errors`; the classifier must not read it Clean.
    let fx = Fixture::new();
    let clone = fx.root.join("github-clone");
    std::fs::create_dir_all(&clone).unwrap();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_github_marketplace_at(&clone);
    let github = Source::GitHub { repo: "owner/repo", ref_: "v0.1.0" };
    let m = marker("github", None, false);
    assert!(matches!(fx.verdict(&plugin(), &github, Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn strictly_newer_github_source_with_missing_manifest_is_needed() {
    // The owned-branch copy of the installLocation manifest check: a strictly-
    // newer github install whose clone lost its manifest is the load failure
    // `probe` reads off `errors`, so the monotonic short-circuit must not read
    // it Clean (the healthy-path term alone would leave the owned no-op Clean).
    let fx = Fixture::new();
    let clone = fx.root.join("github-clone");
    std::fs::create_dir_all(&clone).unwrap();
    fx.seed_installed(Some("0.2.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_github_marketplace_at(&clone);
    let github = Source::GitHub { repo: "owner/repo", ref_: "v0.1.0" };
    let m = marker("github", None, false);
    assert!(matches!(fx.verdict(&plugin(), &github, Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn project_enable_overrides_user_disable_is_needed() {
    // The enable bit is the cwd-merged value: a project settings.json `true`
    // beats a user settings.json `false`, so a drifted tree reads Needed.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(false));
    fx.seed_cwd_settings("settings.json", Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some("different-hash".into()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn local_disable_overrides_user_enable_is_clean() {
    // `settings.local.json` outranks the user settings: an explicit local `false`
    // disables the install, so the disabled row answers Clean even on drift.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_cwd_settings("settings.local.json", Some(false));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some("different-hash".into()), false);
    assert_eq!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Clean);
}

#[test]
fn local_disable_outranks_a_project_enable_and_user_disable_is_clean() {
    // Probe row C: an explicit local `false` wins over a project `true` and a
    // user `false` (settings.local.json > settings.json > <cfg>/settings.json),
    // so a drifted tree under that disable reads Clean, never a repair.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(false));
    fx.seed_cwd_settings("settings.json", Some(true));
    fx.seed_cwd_settings("settings.local.json", Some(false));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some("different-hash".into()), false);
    assert_eq!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Clean);
}

#[test]
fn local_enable_outranks_a_project_disable_is_needed() {
    // The converse of probe row C: an explicit local `true` wins over a project
    // `false`, so the install is enabled and a drifted tree reads Needed.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_cwd_settings("settings.json", Some(false));
    fx.seed_cwd_settings("settings.local.json", Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some("different-hash".into()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn unparseable_project_settings_is_needed() {
    // An unparseable cwd settings layer is "cannot tell" -> Needed.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    std::fs::create_dir_all(fx.cwd.join(".claude")).unwrap();
    std::fs::write(fx.cwd.join(".claude").join("settings.json"), b"{not json").unwrap();
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn unparseable_user_settings_is_needed() {
    // An unparseable `<cfg>/settings.json` is "cannot tell" -> Needed.
    let fx = Fixture::new();
    fx.seed_installed(Some("0.1.0"), Some(&fx.install), "user", None);
    std::fs::write(fx.config.join("settings.json"), b"{not json").unwrap();
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&plugin(), &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}

#[test]
fn version_comparison_uses_the_crate_parser() {
    // `0.1.9` is numerically older than `0.1.10`; a lexicographic compare would
    // read it as newer, so asserting `Needed` pins `version_lt`'s numeric parse.
    let fx = Fixture::new();
    let host = Plugin { name: NAME, marketplace: MKT, version: "0.1.10", agents: &["claude"], instructions: None, blob: &[] };
    fx.seed_installed(Some("0.1.9"), Some(&fx.install), "user", None);
    fx.seed_settings(Some(true));
    fx.seed_marketplace(Some(&fx.pointer()), "directory");
    fx.seed_manifest_at(&fx.pointer());
    let m = marker("embedded", Some(fx.staged_hash()), false);
    assert!(matches!(fx.verdict(&host, &fx.path_source(), Some(&m)), HealNeededVerdict::Needed { .. }));
}
