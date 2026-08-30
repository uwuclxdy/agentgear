//! Hermetic Claude-backend tests, fully isolated from a real `~/.claude`. No
//! docker, no auth, no real `claude`: the `fake_claude` bin is copied onto a
//! scratch `PATH` as `claude`, so the backend's CLI orchestration runs against a
//! throwaway registry while the parts under test are real code writing real files.
//!
//! Two contracts live here: the registry heal cases (a broken/divergent
//! marketplace, an error-carrying plugin entry), and the retired-slot contract —
//! setup/self-heal/uninstall never touch the user's `settings.json`, so a
//! pre-existing statusLine survives byte-identical.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");
const FAKE_CLAUDE: &str = env!("CARGO_BIN_EXE_fake_claude");

/// Mirrors `fake_claude.rs`'s own `STATE_FILENAME`; a bin target exports nothing an
/// external test crate can import, so this is a second literal by necessity, not by
/// oversight (same idiom as `fake_claude_config_dir.rs`'s own copy).
const FAKE_CLAUDE_STATE_FILENAME: &str = "fake-claude-state.json";

/// The user's own settings before we touch anything: an unrelated key plus a real
/// status line of theirs. The automatic slot wiring is retired, so this whole file
/// must survive every lifecycle op byte-identical — nothing this backend writes
/// lives in it anymore.
const SEED_SETTINGS: &str = r#"{
  "theirSetting": true,
  "statusLine": {
    "type": "command",
    "command": "echo their-bar-row",
    "padding": 2
  }
}
"#;

struct Env {
    root: PathBuf,
    cfg: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// `PATH` holding the scratch `claude` double and the fixture binary's dir, and
    /// nothing else — so every other backend's `which` probe stays false regardless
    /// of what the dev box has installed.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-cc-statusline-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let env = Env { cfg: root.join("cfg"), data: root.join("data"), run: root.join("run"), path: curated_path(&bin), root };
        for dir in [&bin, &env.cfg, &env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::copy(FAKE_CLAUDE, bin.join(format!("claude{}", std::env::consts::EXE_SUFFIX))).unwrap();
        fs::write(env.settings_path(), SEED_SETTINGS).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        // `HOME` + `XDG_CONFIG_HOME` under the temp root keep every non-CC backend's
        // config-dir probe false; the curated `PATH` keeps their `which` probe false.
        // Both also pin any write to the sandbox.
        cmd.env("CLAUDE_CONFIG_DIR", &self.cfg)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("PATH", &self.path);
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn settings_path(&self) -> PathBuf {
        self.cfg.join("settings.json")
    }

    fn settings(&self) -> String {
        fs::read_to_string(self.settings_path()).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn curated_path(scratch_bin: &Path) -> OsString {
    let mut dirs = vec![scratch_bin.to_path_buf()];
    if let Some(dir) = Path::new(BIN).parent() {
        dirs.push(dir.to_path_buf());
    }
    std::env::join_paths(&dirs).unwrap_or_default()
}

#[test]
fn claude_setup_leaves_an_existing_user_statusline_untouched() {
    let env = Env::new("untouched");
    let before = env.settings();

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert_eq!(env.settings(), before, "setup rewrote the settings file holding the user's statusLine");

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");
    assert_eq!(env.settings(), before, "self-heal touched the settings file holding the user's statusLine");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(env.settings(), before, "uninstall rewrote the settings file holding the user's statusLine");
}

#[test]
fn claude_broken_marketplace_heal_repoints_and_repairs() {
    let env = Env::new("broken-marketplace");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // Point the registration at a stale dir whose manifest is gone, standing in
    // for the pre-deletion checkout path; the materialized tree stays intact.
    let stale = env.root.join("stale-checkout");
    fs::create_dir_all(stale.join(".claude-plugin")).unwrap();
    fs::write(stale.join(".claude-plugin").join("plugin.json"), "{}").unwrap();
    let state_path = env.cfg.join(FAKE_CLAUDE_STATE_FILENAME);
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    for marketplace in state["marketplaces"].as_array_mut().unwrap() {
        marketplace["path"] = serde_json::json!(stale.to_string_lossy().as_ref());
    }
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored on a broken marketplace: {out}");
    assert_eq!(out, "Repaired", "a marketplace whose manifest vanished must repair, got {out}");

    // The registration was re-pointed at the materialized pointer, not left on
    // the stale dir a `marketplace update` would have failed against.
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let path = state["marketplaces"][0]["path"].as_str().expect("marketplace path");
    let expected = env.data.join("ez-fixture-plugin").join("current@claude");
    assert_eq!(path, expected.to_string_lossy().as_ref(), "the repair must re-point the registration at the materialized tree");

    // Converged: the next heal spends its one read and changes nothing.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "a healed registration must no-op, got {out}");
}

/// The errors-only half of the same heal: the marketplace is healthy and the
/// files resolve, but CC reports a load failure on the plugin entry (seeded into
/// the double's registry, standing in for any CC-side load error the double does
/// not model). probe must classify that `NeedsRepair`, and `structural_ok` must
/// fold the same errors in — without it the reconcile reads the entry healthy and
/// no-ops forever while the probe keeps reporting the break. The repair path
/// reinstalls the entry, which drops the seeded errors, so the next heal is a
/// no-op.
#[test]
fn claude_errors_only_plugin_entry_heals_and_converges() {
    let env = Env::new("errors-only");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let state_path = env.cfg.join(FAKE_CLAUDE_STATE_FILENAME);
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    for plugin in state["plugins"].as_array_mut().unwrap() {
        plugin["errors"] = serde_json::json!(["Marketplace ez-fixture-plugin failed to load: cache-miss"]);
    }
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored on an errors-only entry: {out}");
    assert_eq!(out, "Repaired", "a load-failed entry behind a healthy marketplace must repair, got {out}");

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "the reinstalled entry must no-op, got {out}");
}

/// The still-working half of the migration: the registration points at a
/// directory that still LOADS — its manifest is present — but is not the
/// materialized pointer (the old checkout dir before anyone pulled the deletion).
/// No errors ride the plugin entry, so only `structural_ok`'s divergence check
/// can see the break; without it the heal adopts/NoOps and the registration
/// never converges. This is the case the clauth start pre-flight gate keys on.
#[test]
fn claude_divergent_but_loading_marketplace_heals_and_repoints() {
    let env = Env::new("divergent-loading");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // A stale dir with a VALID manifest: the marketplace loads, so CC computes no
    // errors for it — the divergence is silent everywhere but the pointer check.
    let stale = env.root.join("old-checkout");
    fs::create_dir_all(stale.join(".claude-plugin")).unwrap();
    fs::write(stale.join(".claude-plugin").join("marketplace.json"), "{}").unwrap();
    fs::write(stale.join(".claude-plugin").join("plugin.json"), "{}").unwrap();
    let state_path = env.cfg.join(FAKE_CLAUDE_STATE_FILENAME);
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    for marketplace in state["marketplaces"].as_array_mut().unwrap() {
        marketplace["path"] = serde_json::json!(stale.to_string_lossy().as_ref());
    }
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored on a divergent-but-loading marketplace: {out}");
    assert_eq!(out, "Repaired", "a registration elsewhere than the pointer must converge, got {out}");

    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let path = state["marketplaces"][0]["path"].as_str().expect("marketplace path");
    let expected = env.data.join("ez-fixture-plugin").join("current@claude");
    assert_eq!(path, expected.to_string_lossy().as_ref(), "the repair must re-point the loading registration at the materialized tree");
}

/// The scope filter's discriminating case, the standing failure it was built
/// from: a dead PROJECT-scope entry listed FIRST while the user-scope entry is
/// healthy. A scope-blind probe reads the project entry, classifies
/// `NeedsRepair`, and the user-scope repair then churns against the entry it can
/// never fix. The filtered probe reads the user entry and no-ops. Seeded by hand
/// because the double is scope-blind by design — every entry IT installs is
/// user-scope, so only a hand-written registry entry can pose the other scope.
#[test]
fn claude_heal_ignores_a_dead_project_entry_at_user_scope() {
    let env = Env::new("project-entry-first");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let state_path = env.cfg.join(FAKE_CLAUDE_STATE_FILENAME);
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let dead_project = serde_json::json!({
        "id": "ez-fixture-plugin@ez-fixture-plugin",
        "enabled": true,
        "scope": "project",
        "installPath": "/gone/runtime/plugins/cache",
        "errors": ["Marketplace ez-fixture-plugin failed to load: cache-miss"]
    });
    let plugins = state["plugins"].as_array_mut().unwrap();
    plugins.insert(0, dead_project);
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored with a dead project entry present: {out}");
    assert_eq!(out, "NoOp", "a dead project entry must not drive the user-scope heal, got {out}");
}
