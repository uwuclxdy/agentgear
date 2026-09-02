//! Hermetic copilot-cli tests, fully isolated from a real `~/.copilot`. No
//! docker, no auth, no real `copilot`: the `fake_copilot` bin is copied onto a
//! scratch `PATH` as `copilot`, so the backend's CLI orchestration runs against a
//! throwaway registry while the parts under test are real code writing real files.
//!
//! Two contracts live here: the same-version tree refresh (copilot's install copy
//! is keyed on the plugin, so only a reinstall replaces it), and the retired-slot
//! contract — setup/self-heal/uninstall never touch the user's `settings.json`, so
//! a pre-existing statusLine survives byte-identical.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");
const FAKE_COPILOT: &str = env!("CARGO_BIN_EXE_fake_copilot");

/// The user's own settings: an unrelated key plus a real status line of theirs. The
/// automatic slot wiring is retired, so this whole file must survive every
/// lifecycle op byte-identical — nothing this backend writes lives in it anymore.
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
    /// `$COPILOT_HOME` — the config dir holding the user's `settings.json` and the
    /// double's own registry state.
    copilot_home: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// `PATH` holding the scratch `copilot` double and the fixture binary's dir, and
    /// nothing else — so every other backend's `which` probe stays false regardless of
    /// what the dev box has installed.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-copilot-statusline-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let env =
            Env { copilot_home: root.join("copilot"), data: root.join("data"), run: root.join("run"), path: curated_path(&bin), root };
        for dir in [&bin, &env.copilot_home, &env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::copy(FAKE_COPILOT, bin.join(format!("copilot{}", std::env::consts::EXE_SUFFIX))).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        // `HOME` + `XDG_CONFIG_HOME` under the temp root keep every other backend's
        // config-dir probe false; the curated `PATH` keeps their `which` probe false.
        // Both also pin any write to the sandbox. `COPILOT_HOME` is what the backend
        // (and the double) resolve their config dir from, ahead of `HOME`.
        cmd.env("COPILOT_HOME", &self.copilot_home)
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
        self.copilot_home.join("settings.json")
    }

    fn seed_settings(&self) {
        fs::write(self.settings_path(), SEED_SETTINGS).unwrap();
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
fn copilot_cli_setup_leaves_an_existing_user_statusline_untouched() {
    let env = Env::new("untouched");
    env.seed_settings();
    let before = env.settings();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
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
fn copilot_cli_teardown_with_nothing_installed_writes_no_file() {
    // A teardown that owns nothing must not create a settings file in the user's config
    // dir at all.
    let env = Env::new("empty-teardown");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall errored with nothing installed: {out}");
    assert_eq!(out, "NoOp", "a teardown with nothing of ours to undo must report no change, got {out}");
    assert!(!env.settings_path().exists(), "teardown created a settings file it had nothing to undo in");
}

/// Mirrors `fake_copilot.rs`'s own `CALL_LOG_FILENAME`, for the same reason the state
/// file's name is duplicated above.
const FAKE_COPILOT_CALL_LOG: &str = "fake-copilot-calls.log";

/// The fixture plugin's own name, which is also its data-root directory.
const PLUGIN_NAME: &str = "ez-fixture-plugin";

fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&entry.path(), &dst_path);
        } else {
            fs::copy(entry.path(), &dst_path).unwrap();
        }
    }
}

#[test]
fn copilot_cli_reinstalls_a_same_version_tree_change_and_nothing_else() {
    // copilot's install copy is keyed on the plugin, its freshness on the version, so a
    // tree edited at an unchanged version is invisible to every version comparison the
    // registry offers and copilot keeps serving the bytes it first copied. Only a
    // reinstall replaces that copy, and the registry cannot show one — uninstall +
    // install of the same id leaves the state that was already there — so the double's
    // call log is the observation.
    let env = Env::new("tree-refresh");
    let src = env.root.join("src-plugin");
    copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &src);
    let source = src.to_str().unwrap().to_string();
    let hello = src.join("commands").join("hello.md");
    let staged = env.data.join(PLUGIN_NAME).join("current@copilot-cli").join("commands").join("hello.md");
    let calls = || fs::read_to_string(env.copilot_home.join(FAKE_COPILOT_CALL_LOG)).unwrap_or_default();
    let uninstalls = |log: &str| log.lines().filter(|l| l.starts_with("plugin uninstall")).count();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok, "first setup failed: {out}");
    assert_eq!(out, "Installed", "first setup did not install");
    let after_install = calls();

    // An unchanged tree must not churn: without this leg the refresh below passes for
    // any binary that simply reinstalls every session.
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok, "second setup failed: {out}");
    assert_eq!(out, "NoOp", "an unchanged tree at an unchanged version must converge to a no-op");
    assert_eq!(uninstalls(&calls()), uninstalls(&after_install), "an unchanged tree must not reinstall:\n{}", calls());

    let original = fs::read_to_string(&hello).unwrap();
    fs::write(&hello, format!("{original}\n<!-- same-version-edit -->\n")).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok, "third setup failed: {out}");
    let staged_body = fs::read_to_string(&staged).unwrap();
    assert!(staged_body.contains("same-version-edit"), "the edit never reached the staged tree:\n{staged_body}");
    assert_eq!(uninstalls(&calls()), uninstalls(&after_install) + 1, "a changed tree must reinstall exactly once:\n{}", calls());
    assert_eq!(out, "Repaired", "a re-copied tree is a repair, not a no-op");
}

#[test]
fn copilot_cli_re_hands_the_tree_to_an_unowned_install() {
    // The same class claude carries: the tree hash lives in the marker, so an install
    // this binary never made leaves copilot's copy unaccounted for while the registry
    // entry still reads correct. Adopting on that read alone stamps ownership over bytes
    // nothing ever compared, and the (marker present, Healthy) row no-ops over them from
    // then on.
    let env = Env::new("unowned");
    let calls = || fs::read_to_string(env.copilot_home.join(FAKE_COPILOT_CALL_LOG)).unwrap_or_default();
    let uninstalls = |log: &str| log.lines().filter(|l| l.starts_with("plugin uninstall")).count();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "setup did not install");
    let after_install = calls();

    fs::remove_dir_all(env.data.join(PLUGIN_NAME).join("markers")).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed on an unowned install: {out}");
    assert_eq!(out, "Repaired", "an unowned install must be re-handed its tree, got {out}");
    // Copilot's copy is version-keyed the same way CC's is, so the uninstall half is
    // what proves the tree was handed over rather than merely re-registered.
    assert_eq!(uninstalls(&calls()), uninstalls(&after_install) + 1, "the takeover never re-handed the tree:\n{}", calls());

    // The takeover records the tree, so the next session no-ops.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "the settling heal failed: {out}");
    assert_eq!(out, "NoOp", "a recorded takeover must converge to a no-op");
}
