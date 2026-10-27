//! Hermetic copilot-cli tests, fully isolated from a real `~/.copilot`. No
//! docker, no auth, no real `copilot`: the `fake_copilot` bin is staged onto a
//! scratch `PATH` as `copilot`, so the backend's CLI orchestration runs against a
//! throwaway registry while the parts under test are real code writing real files.
//!
//! Two contracts live here: the same-version tree refresh (a local-marketplace
//! plugin is served live, never copied, so re-staging the `current@copilot-cli`
//! pointer alone re-hands an edited tree — never an uninstall+install), and the
//! retired-slot contract — setup/self-heal/uninstall never touch the user's
//! `settings.json`, so a pre-existing statusLine survives byte-identical.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");
const FAKE_COPILOT: &str = env!("CARGO_BIN_EXE_fake_copilot");
const GITHUB_FIXTURE: &str = env!("CARGO_BIN_EXE_github_fixture");

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
    /// The version the `copilot` double reports and models (`FAKE_COPILOT_VERSION`).
    fake_version: &'static str,
    /// The plugin version the double reports in `plugin list` rows
    /// (`FAKE_COPILOT_PLUGIN_VERSION`); `None` omits the `(v…)` column.
    fake_plugin_version: Option<&'static str>,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-copilot-statusline-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let env = Env {
            copilot_home: root.join("copilot"),
            data: root.join("data"),
            run: root.join("run"),
            path: curated_path(&bin),
            root,
            fake_version: "1.0.92",
            fake_plugin_version: None,
        };
        for dir in [&bin, &env.copilot_home, &env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        stage_double(FAKE_COPILOT, &bin.join(format!("copilot{}", std::env::consts::EXE_SUFFIX)));
        env
    }

    /// Override the version the double reports (the default `1.0.92` already
    /// models disable-only uninstall and the `config` subcommand).
    fn fake_version(mut self, version: &'static str) -> Self {
        self.fake_version = version;
        self
    }

    /// Make the double report a `(v…)` column, so a test can drive the Update or
    /// Frozen reconcile arms (a version column the double otherwise omits).
    fn fake_plugin_version(mut self, version: &'static str) -> Self {
        self.fake_plugin_version = Some(version);
        self
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
            .env("FAKE_COPILOT_VERSION", self.fake_version)
            .env("PATH", &self.path);
        match self.fake_plugin_version {
            Some(v) => {
                cmd.env(FAKE_COPILOT_PLUGIN_VERSION, v);
            }
            None => {
                cmd.env_remove(FAKE_COPILOT_PLUGIN_VERSION);
            }
        }
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Like [`Env::fixture`] but also returns stderr, for the error-path tests
    /// whose fix hint rides on stderr.
    fn fixture_err(&self, args: &[&str]) -> (bool, String, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }

    /// Like [`Env::fixture_err`] but passes extra env to the host, which forwards it
    /// to the double it spawns.
    fn fixture_err_with(&self, args: &[&str], extra: &[(&str, &str)]) -> (bool, String, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        for (key, value) in extra {
            cmd.env(key, value);
        }
        let out = cmd.output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }

    /// Runs the github-default fixture host (same sandbox, same double).
    fn github_fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(GITHUB_FIXTURE);
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

/// Stages a CLI double onto the scratch `PATH`: a symlink on unix, a copy
/// elsewhere. A copy opens the destination for write, which races a sibling
/// test's exec of it (`ETXTBSY`); the symlink never write-opens the inode the
/// child execs. The link points at the build artifact, so a test needing its
/// own variant must remove the link and copy in a replacement, never write
/// through the link.
#[cfg(unix)]
fn stage_double(src: &str, dst: &Path) {
    std::os::unix::fs::symlink(src, dst).unwrap();
}

#[cfg(not(unix))]
fn stage_double(src: &str, dst: &Path) {
    fs::copy(src, dst).unwrap();
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
    assert!(!env.copilot_home.join("settings.json.lock").exists(), "teardown created a config lock file it had nothing to clear");
}

#[test]
fn copilot_cli_uninstall_refuses_a_copilot_below_the_floor() {
    let env = Env::new("too-old").fake_version("1.0.91");
    let (ok, _out, err) = env.fixture_err(&["uninstall"]);
    assert!(!ok, "uninstall must refuse a copilot below the 1.0.92 floor");
    assert_eq!(
        err,
        "error: copilot-cli: copilot >= 1.0.92 required for plugin management (found GitHub Copilot CLI 1.0.91.); run `copilot update`"
    );
}

/// The always-loaded guidance block is stripped before the version gate can refuse a
/// below-floor copilot, so an uninstall never strands guidance.
#[test]
fn copilot_cli_uninstall_strips_instructions_before_refusing_a_below_floor_copilot() {
    let env = Env::new("too-old-instr").fake_version("1.0.91");
    let instructions = env.copilot_home.join("copilot-instructions.md");
    let seed = "user line\n";
    fs::write(&instructions, format!("{seed}\n{}", instr_block())).unwrap();

    let (ok, _out, err) = env.fixture_err(&["uninstall"]);
    assert!(!ok, "uninstall must refuse a copilot below the 1.0.92 floor");
    assert_eq!(
        err,
        "error: copilot-cli: copilot >= 1.0.92 required for plugin management (found GitHub Copilot CLI 1.0.91.); run `copilot update`"
    );
    assert_eq!(fs::read_to_string(&instructions).unwrap(), seed, "the guidance block must be stripped even below the floor");
}

/// The disable-only teardown: at >= 1.0.81 `plugin uninstall` only disables (the row
/// stays, `enabledPlugins[id]` flips false), so a remove that stops there leaves our
/// plugin listed and reports `Removed` falsely. The backend must follow with
/// `marketplace remove` + `config --rm` and leave the double's registry empty.
#[test]
fn copilot_cli_uninstall_removes_a_disable_only_plugin_and_its_marketplace() {
    let env = Env::new("disable-only-teardown").fake_version("1.0.104");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(env.copilot_home.join("fake-copilot-state.json")).unwrap()).unwrap();
    assert_eq!(state["plugins"], serde_json::json!([]), "our plugin must be gone from the registry:\n{state}");
    assert_eq!(state["marketplaces"], serde_json::json!([]), "our marketplace must be removed:\n{state}");
    assert_eq!(state["enabledPlugins"], serde_json::json!({}), "no enabledPlugins key may name our id:\n{state}");
}

/// A deliberate `plugin disable` must survive self_heal (probe reads `Disabled`,
/// the marker-present row no-ops before reconcile) while an explicit `setup`
/// re-enables it through `plugin enable`.
#[test]
fn copilot_cli_self_heal_preserves_a_disabled_plugin_but_explicit_setup_reenables() {
    let env = Env::new("disabled-preserve");
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let read_state = || serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&state_path).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // The user disables our plugin by hand, through copilot's own subcommand.
    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["plugin", "disable", "ez-fixture-plugin@ez-fixture-plugin"]);
    env.apply(&mut cmd);
    assert!(cmd.output().unwrap().status.success(), "the fake disable failed");

    // self-heal (marker present, probe -> Disabled) must no-op, never re-enable.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");
    assert_eq!(out, "NoOp", "self-heal must not re-enable a deliberately disabled plugin, got {out}");
    let doc = read_state();
    assert_eq!(doc["plugins"][0]["disabled"], serde_json::json!(true), "the disable must survive self-heal");

    // An explicit setup re-enables.
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "re-setup failed: {out}");
    assert_eq!(out, "Repaired", "explicit setup must re-enable a disabled plugin, got {out}");
    let doc = read_state();
    assert_eq!(doc["plugins"][0]["disabled"], serde_json::json!(false), "explicit setup did not re-enable");
}

/// An explicit `setup` over a disabled, strictly-newer install re-enables it before
/// the frozen check (like claude), so the user's explicit request is never dropped.
#[test]
fn copilot_cli_explicit_setup_reenables_a_frozen_disabled_install() {
    let env = Env::new("frozen-disabled").fake_plugin_version("0.1.6");
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let read_state = || serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&state_path).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["plugin", "disable", "ez-fixture-plugin@ez-fixture-plugin"]);
    env.apply(&mut cmd);
    assert!(cmd.output().unwrap().status.success(), "the fake disable failed");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "re-setup failed: {out}");
    assert_eq!(out, "Repaired", "an explicit setup must re-enable even a frozen disabled install, got {out}");
    let doc = read_state();
    assert_eq!(doc["plugins"][0]["disabled"], serde_json::json!(false), "the frozen row stayed disabled");
}

/// An explicit `setup` over a disabled, stale install re-enables and updates it.
#[test]
fn copilot_cli_explicit_setup_reenables_and_updates_a_disabled_stale_install() {
    let env = Env::new("stale-disabled").fake_plugin_version("0.1.4");
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let read_state = || serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&state_path).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["plugin", "disable", "ez-fixture-plugin@ez-fixture-plugin"]);
    env.apply(&mut cmd);
    assert!(cmd.output().unwrap().status.success(), "the fake disable failed");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "re-setup failed: {out}");
    assert_eq!(out, r#"Updated { from: Some("0.1.4"), to: "0.1.5" }"#, "a disabled stale install must update, got {out}");
    let doc = read_state();
    assert_eq!(doc["plugins"][0]["disabled"], serde_json::json!(false), "the stale row stayed disabled");
}

/// An explicit `setup` over a disabled install whose tree changed at the same version
/// re-enables it and refreshes the tree.
#[test]
fn copilot_cli_explicit_setup_reenables_a_disabled_install_with_a_changed_tree() {
    let env = Env::new("refresh-disabled").fake_plugin_version("0.1.5");
    let src = env.root.join("src-plugin");
    copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &src);
    let source = src.to_str().unwrap().to_string();
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let read_state = || serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&state_path).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let hello = src.join("commands").join("hello.md");
    let original = fs::read_to_string(&hello).unwrap();
    fs::write(&hello, format!("{original}\n<!-- same-version-edit -->\n")).unwrap();

    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["plugin", "disable", "ez-fixture-plugin@ez-fixture-plugin"]);
    env.apply(&mut cmd);
    assert!(cmd.output().unwrap().status.success(), "the fake disable failed");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok, "re-setup failed: {out}");
    assert_eq!(out, "Repaired", "a disabled install with a changed tree must refresh, got {out}");
    let doc = read_state();
    assert_eq!(doc["plugins"][0]["disabled"], serde_json::json!(false), "the refreshed row stayed disabled");
}

/// A first setup whose `plugin install` fails after `marketplace add` leaves a
/// `(disabled)` row and no marker. self_heal must converge that half-install — a
/// markerless `(disabled)` row is not a recorded user disable — never preserve it
/// as a disable forever.
#[test]
fn copilot_cli_self_heal_converges_a_half_install_whose_install_failed() {
    let env = Env::new("half-install");
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let read_state = || serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&state_path).unwrap()).unwrap();

    let (ok, _out, _err) = env.fixture_err_with(&["setup", "--agent", "copilot-cli"], &[("FAKE_COPILOT_REFUSE_INSTALL", "1")]);
    assert!(!ok, "the refused setup must fail");

    let doc = read_state();
    assert_eq!(doc["plugins"][0]["id"], serde_json::json!("ez-fixture-plugin@ez-fixture-plugin"), "the add must list our plugin:\n{doc}");
    assert_eq!(doc["plugins"][0]["disabled"], serde_json::json!(true), "the refused install leaves a (disabled) row:\n{doc}");
    assert!(!env.data.join(PLUGIN_NAME).join("markers").exists(), "the failed setup must not write a marker");

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed to converge the half-install: {out}");
    assert_eq!(out, "Repaired", "self-heal must repair the half-install, got {out}");
    let doc = read_state();
    assert_eq!(doc["plugins"][0]["disabled"], serde_json::json!(false), "self-heal must enable the half-install:\n{doc}");
}

/// A user's `plugin disable` survives self_heal even with no marker of ours (a
/// marker an uninstall cleared while copilot was off PATH, or a hand install):
/// copilot's own `enabledPlugins` key reads `false`, which self_heal must never
/// re-enable.
#[test]
fn copilot_cli_self_heal_preserves_a_markerless_user_disable() {
    let env = Env::new("markerless-disable");
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let read_state = || serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&state_path).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["plugin", "disable", "ez-fixture-plugin@ez-fixture-plugin"]);
    env.apply(&mut cmd);
    assert!(cmd.output().unwrap().status.success(), "the fake disable failed");
    fs::remove_dir_all(env.data.join(PLUGIN_NAME).join("markers")).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");
    assert_eq!(out, "NoOp", "self-heal must not re-enable a user's disable, got {out}");
    assert_eq!(read_state()["plugins"][0]["disabled"], serde_json::json!(true), "the user's disable must survive self-heal");
}

/// doctor names a disabled row, so a registered-but-disabled plugin never reads as
/// fully healthy (claude's doctor precedent).
#[test]
fn copilot_cli_doctor_names_a_disabled_row() {
    let env = Env::new("doctor-disabled");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["plugin", "disable", "ez-fixture-plugin@ez-fixture-plugin"]);
    env.apply(&mut cmd);
    assert!(cmd.output().unwrap().status.success(), "the fake disable failed");

    let (ok, out) = env.fixture(&["doctor"]);
    assert!(ok, "doctor failed: {out}");
    assert!(
        out.lines().any(|l| l == "[ ok ] plugin registered: ez-fixture-plugin@ez-fixture-plugin v? (disabled)"),
        "doctor must name a disabled row, got:\n{out}"
    );
}

/// Mirrors `fake_copilot.rs`'s own `CALL_LOG_FILENAME`, for the same reason the state
/// file's name is duplicated above.
const FAKE_COPILOT_CALL_LOG: &str = "fake-copilot-calls.log";

/// Mirrors `fake_copilot.rs`'s `PLUGIN_VERSION_ENV`.
const FAKE_COPILOT_PLUGIN_VERSION: &str = "FAKE_COPILOT_PLUGIN_VERSION";

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

/// How many stamp markers this agent holds (a cleared marker leaves the empty
/// `markers/` dir, so "no marker" is an entry count, never the dir's absence).
fn marker_count(env: &Env) -> usize {
    fs::read_dir(env.data.join(PLUGIN_NAME).join("markers")).map(|d| d.count()).unwrap_or(0)
}

#[test]
fn copilot_cli_restages_a_same_version_tree_change_on_setup_without_reinstall() {
    // A local-marketplace plugin is served live from its source dir (copilot never
    // copies it), but its version column is unchanged by a same-version tree edit, so
    // every version comparison the registry offers is blind to the edit. Re-staging the
    // pointer is the backend's convergence signal, and the registry cannot show one —
    // re-staging leaves the state that was already there — so the double's call log is
    // the observation: no `plugin uninstall`, no `plugin install` after the first setup.
    let env = Env::new("tree-restage");
    let src = env.root.join("src-plugin");
    copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &src);
    let source = src.to_str().unwrap().to_string();
    let hello = src.join("commands").join("hello.md");
    let staged = env.data.join(PLUGIN_NAME).join("current@copilot-cli").join("commands").join("hello.md");
    let calls = || fs::read_to_string(env.copilot_home.join(FAKE_COPILOT_CALL_LOG)).unwrap_or_default();
    let count = |log: &str, prefix: &str| log.lines().filter(|l| l.starts_with(prefix)).count();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok, "first setup failed: {out}");
    assert_eq!(out, "Installed", "first setup did not install");
    let after_install = calls();

    // An unchanged tree must not churn: without this leg the restage below passes for
    // any binary that simply re-stages every session.
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok, "second setup failed: {out}");
    assert_eq!(out, "NoOp", "an unchanged tree at an unchanged version must converge to a no-op");
    assert_eq!(
        count(&calls(), "plugin install"),
        count(&after_install, "plugin install"),
        "an unchanged tree must not install:\n{}",
        calls()
    );

    let original = fs::read_to_string(&hello).unwrap();
    fs::write(&hello, format!("{original}\n<!-- same-version-edit -->\n")).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok, "third setup failed: {out}");
    let staged_body = fs::read_to_string(&staged).unwrap();
    assert!(staged_body.contains("same-version-edit"), "the edit never reached the staged tree:\n{staged_body}");
    assert_eq!(
        count(&calls(), "plugin uninstall"),
        count(&after_install, "plugin uninstall"),
        "a changed tree must not uninstall:\n{}",
        calls()
    );
    assert_eq!(count(&calls(), "plugin install"), count(&after_install, "plugin install"), "a changed tree must not install:\n{}", calls());
    assert_eq!(out, "Repaired", "a re-staged tree is a repair, not a no-op");
}

#[test]
fn copilot_cli_re_hands_the_tree_to_an_unowned_install() {
    // The same class claude carries: the tree hash lives in the marker, so an install
    // this binary never made leaves the tree copilot serves unaccounted for while the registry
    // entry still reads correct. Adopting on that read alone stamps ownership over bytes
    // nothing ever compared, and the (marker present, Healthy) row no-ops over them from
    // then on.
    let env = Env::new("unowned");
    let calls = || fs::read_to_string(env.copilot_home.join(FAKE_COPILOT_CALL_LOG)).unwrap_or_default();
    let count = |log: &str, prefix: &str| log.lines().filter(|l| l.starts_with(prefix)).count();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "setup did not install");
    let after_install = calls();

    fs::remove_dir_all(env.data.join(PLUGIN_NAME).join("markers")).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed on an unowned install: {out}");
    assert_eq!(out, "Repaired", "an unowned install must be re-handed its tree, got {out}");
    // The takeover re-stages the pointer alone, so no registry call is the proof the
    // tree was handed over rather than merely re-registered.
    assert_eq!(
        count(&calls(), "plugin uninstall"),
        count(&after_install, "plugin uninstall"),
        "the takeover must not uninstall:\n{}",
        calls()
    );
    assert_eq!(count(&calls(), "plugin install"), count(&after_install, "plugin install"), "the takeover must not install:\n{}", calls());

    // The takeover records the tree, so the next session no-ops.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "the settling heal failed: {out}");
    assert_eq!(out, "NoOp", "a recorded takeover must converge to a no-op");
}

/// A same-version tree change on a `--path` source converges on self-heal by
/// re-staging the `current@copilot-cli` pointer alone: no `plugin uninstall`, no
/// `plugin install`, the row never reads `(disabled)`, and the staged tree holds
/// the edit.
#[test]
fn copilot_cli_restages_a_same_version_tree_change_on_self_heal_without_reinstall() {
    let env = Env::new("restage-self-heal");
    let src = env.root.join("src-plugin");
    copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &src);
    let source = src.to_str().unwrap().to_string();
    let hello = src.join("commands").join("hello.md");
    let staged = env.data.join(PLUGIN_NAME).join("current@copilot-cli").join("commands").join("hello.md");
    let calls = || fs::read_to_string(env.copilot_home.join(FAKE_COPILOT_CALL_LOG)).unwrap_or_default();
    let count = |log: &str, prefix: &str| log.lines().filter(|l| l.starts_with(prefix)).count();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    let after_install = calls();

    let original = fs::read_to_string(&hello).unwrap();
    fs::write(&hello, format!("{original}\n<!-- same-version-edit -->\n")).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");
    assert_eq!(out, "Repaired", "self-heal must repair a changed tree, got {out}");
    assert_eq!(
        count(&calls(), "plugin uninstall"),
        count(&after_install, "plugin uninstall"),
        "self-heal must not uninstall:\n{}",
        calls()
    );
    assert_eq!(count(&calls(), "plugin install"), count(&after_install, "plugin install"), "self-heal must not install:\n{}", calls());

    let staged_body = fs::read_to_string(&staged).unwrap();
    assert!(staged_body.contains("same-version-edit"), "the edit never reached the staged tree:\n{staged_body}");

    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(env.copilot_home.join("fake-copilot-state.json")).unwrap()).unwrap();
    assert_eq!(state["plugins"][0]["disabled"], serde_json::json!(false), "the row must never read (disabled):\n{state}");
}

/// A version bump on a local source converges by re-staging the pointer: the staged
/// tree holds the new bytes, and no `plugin uninstall` / `plugin install` runs.
#[test]
fn copilot_cli_restages_a_version_bump_without_reinstall() {
    let env = Env::new("restage-bump").fake_plugin_version("0.1.4");
    let src = env.root.join("src-plugin");
    copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &src);
    let source = src.to_str().unwrap().to_string();
    let hello = src.join("commands").join("hello.md");
    let staged = env.data.join(PLUGIN_NAME).join("current@copilot-cli").join("commands").join("hello.md");
    let calls = || fs::read_to_string(env.copilot_home.join(FAKE_COPILOT_CALL_LOG)).unwrap_or_default();
    let count = |log: &str, prefix: &str| log.lines().filter(|l| l.starts_with(prefix)).count();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    let after_install = calls();

    let original = fs::read_to_string(&hello).unwrap();
    fs::write(&hello, format!("{original}\n<!-- bump-edit -->\n")).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok, "update setup failed: {out}");
    assert_eq!(out, r#"Updated { from: Some("0.1.4"), to: "0.1.5" }"#);
    assert!(fs::read_to_string(&staged).unwrap().contains("bump-edit"), "the version bump never re-staged the tree");
    assert_eq!(
        count(&calls(), "plugin uninstall"),
        count(&after_install, "plugin uninstall"),
        "an update must not uninstall:\n{}",
        calls()
    );
    assert_eq!(count(&calls(), "plugin install"), count(&after_install, "plugin install"), "an update must not install:\n{}", calls());
}

/// The round-4 interrupted-refresh scenario collapses to a plain re-stage: with no
/// uninstall in the path there is no interrupted-refresh state, so an edited tree
/// converges on self-heal alone — no uninstall, no install, the row stays enabled,
/// and the staged tree holds the latest edit.
#[test]
fn a_tree_edit_converges_on_self_heal_without_a_reinstall_state() {
    let env = Env::new("tree-edit-lock");
    let src = env.root.join("src-plugin");
    copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &src);
    let source = src.to_str().unwrap().to_string();
    let hello = src.join("commands").join("hello.md");
    let staged = env.data.join(PLUGIN_NAME).join("current@copilot-cli").join("commands").join("hello.md");
    let calls = || fs::read_to_string(env.copilot_home.join(FAKE_COPILOT_CALL_LOG)).unwrap_or_default();
    let count = |log: &str, prefix: &str| log.lines().filter(|l| l.starts_with(prefix)).count();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    let after_install = calls();

    let original = fs::read_to_string(&hello).unwrap();
    fs::write(&hello, format!("{original}\n<!-- T1 -->\n")).unwrap();
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok && out == "Repaired", "T1 re-stage failed: {out}");
    assert!(fs::read_to_string(&staged).unwrap().contains("T1"), "T1 was not staged");

    // A second edit converges on self-heal alone.
    fs::write(&hello, format!("{original}\n<!-- T2 -->\n")).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed on the T2 edit: {out}");
    assert_eq!(out, "Repaired", "the T2 self-heal must repair, got {out}");
    assert!(fs::read_to_string(&staged).unwrap().contains("T2"), "copilot is left serving T1");
    assert_eq!(
        count(&calls(), "plugin uninstall"),
        count(&after_install, "plugin uninstall"),
        "self-heal must not uninstall:\n{}",
        calls()
    );
    assert_eq!(count(&calls(), "plugin install"), count(&after_install, "plugin install"), "self-heal must not install:\n{}", calls());

    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(env.copilot_home.join("fake-copilot-state.json")).unwrap()).unwrap();
    assert_eq!(state["plugins"][0]["disabled"], serde_json::json!(false), "the row must never read (disabled):\n{state}");
}

/// A re-stage reaches copilot only through the marketplace registered at this
/// host's `current@copilot-cli` pointer. A legacy plain `current` under our data
/// root cannot be refreshed by a re-stage (it flips only `current@copilot-cli`),
/// so self-heal must fail loud instead of reporting `Repaired` over a stale tree.
#[test]
fn copilot_cli_self_heal_fails_loud_when_marketplace_is_registered_at_a_legacy_current() {
    let env = Env::new("served-legacy");
    let src = env.root.join("src-plugin");
    copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &src);
    let source = src.to_str().unwrap().to_string();
    let hello = src.join("commands").join("hello.md");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // Re-register our marketplace's name at the pre-scoping plain `current` under
    // our own data root: the layout an install from before client-scoping left.
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let mut state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let registered = env.data.join(PLUGIN_NAME).join("current");
    state["marketplaces"][0]["path"] = serde_json::json!(registered);
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let original = fs::read_to_string(&hello).unwrap();
    fs::write(&hello, format!("{original}\n<!-- served-legacy-edit -->\n")).unwrap();

    let (ok, _out, err) = env.fixture_err(&["self-heal"]);
    assert!(!ok, "self-heal must fail loud when our marketplace is not at our pointer");
    let ours = env.data.join(PLUGIN_NAME).join("current@copilot-cli");
    let want = format!(
        "error: copilot-cli: ez-fixture-plugin@ez-fixture-plugin is served from marketplace `ez-fixture-plugin` registered at `{}`, not this host's `{}`, so copilot never sees the re-staged tree; run the host binary's `uninstall` subcommand (it drops the legacy registration), then re-run setup",
        registered.display(),
        ours.display()
    );
    assert_eq!(err, want, "the error must name the registration, our pointer, and the fix");

    // The failed heal must not record a convergence, so the next heal re-derives
    // the same break instead of no-oping over the stale tree.
    let (ok, _out, err_again) = env.fixture_err(&["self-heal"]);
    assert!(!ok, "the failed heal must not record convergence, so the next heal must still fail");
    assert_eq!(err_again, err, "the repeated heal must name the same registration");
}

/// A same-named marketplace registered at a path this host never added (a user's
/// hand `marketplace add` after `marketplace remove` of ours) is not ours: the
/// row it lists is the user's own, so self-heal clears our marker without
/// touching it, and uninstall drops only our marker + guidance, never the
/// foreign row or marketplace.
#[test]
fn copilot_cli_self_heal_clears_our_marker_over_a_foreign_registration() {
    let env = Env::new("served-foreign");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let read_state = || serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let mut state = read_state();
    state["marketplaces"][0]["path"] = serde_json::json!("/elsewhere");
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    let state_before = fs::read(&state_path).unwrap();
    assert!(env.data.join(PLUGIN_NAME).join("markers").exists(), "precondition: our marker is present");

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal must not fail over a foreign registration: {out}");
    assert_eq!(out, "Cleared", "a marker over a foreign row clears, got {out}");
    assert_eq!(marker_count(&env), 0, "the marker must be cleared");
    assert_eq!(fs::read(&state_path).unwrap(), state_before, "self-heal must not mutate the foreign row or marketplace");

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "the settling heal must not fail: {out}");
    assert_eq!(out, "NoOp", "a markerless heal over a foreign row no-ops, got {out}");

    // Uninstall over a foreign registration: our marker is already gone; our
    // guidance block is ours to strip, the foreign marketplace and row stay.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall must not fail over a foreign registration: {out}");
    assert_eq!(out, "Removed", "uninstall strips our guidance, got {out}");
    let doc = read_state();
    assert_eq!(doc["marketplaces"][0]["path"], serde_json::json!("/elsewhere"), "the foreign marketplace must survive:\n{doc}");
    assert_eq!(doc["plugins"][0]["disabled"], serde_json::json!(false), "the foreign row must stay enabled:\n{doc}");
    assert_eq!(marker_count(&env), 0, "uninstall must clear our marker");
}

/// A same-named marketplace the user added AND installed from (keyed `true`,
/// enabled row) with no marker of ours is not ours: self-heal must no-op and
/// write nothing.
#[test]
fn copilot_cli_self_heal_leaves_a_foreign_keyed_enabled_marketplace_alone() {
    let env = Env::new("foreign-keyed-enabled");
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    env.seed_settings();
    let settings_before = env.settings();

    let seed = serde_json::json!({
        "plugins": [{"id": "ez-fixture-plugin@ez-fixture-plugin", "disabled": false}],
        "marketplaces": [{"name": "ez-fixture-plugin", "path": "/tmp/foreign"}],
        "enabledPlugins": {"ez-fixture-plugin@ez-fixture-plugin": true},
    });
    fs::write(&state_path, serde_json::to_vec_pretty(&seed).unwrap()).unwrap();
    let state_before = fs::read(&state_path).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal must not fail over a foreign keyed listing: {out}");
    assert_eq!(out, "NoOp", "a foreign keyed marketplace is not ours to converge, got {out}");
    assert_eq!(fs::read(&state_path).unwrap(), state_before, "self-heal must not mutate a foreign keyed listing");
    assert_eq!(env.settings(), settings_before, "self-heal must not touch the user's settings");
    assert!(!env.data.join(PLUGIN_NAME).join("markers").exists(), "self-heal must not write a marker for a foreign keyed listing");
}

/// A same-named marketplace the user installed then disabled (keyed `false`,
/// `(disabled)` row) with no marker is not ours: self-heal must no-op without
/// writing a marker, never re-enable it.
#[test]
fn copilot_cli_self_heal_leaves_a_foreign_disabled_keyed_marketplace_alone() {
    let env = Env::new("foreign-keyed-disabled");
    let state_path = env.copilot_home.join("fake-copilot-state.json");

    let seed = serde_json::json!({
        "plugins": [{"id": "ez-fixture-plugin@ez-fixture-plugin", "disabled": true}],
        "marketplaces": [{"name": "ez-fixture-plugin", "path": "/tmp/foreign"}],
        "enabledPlugins": {"ez-fixture-plugin@ez-fixture-plugin": false},
    });
    fs::write(&state_path, serde_json::to_vec_pretty(&seed).unwrap()).unwrap();
    let state_before = fs::read(&state_path).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal must not fail over a foreign disabled keyed listing: {out}");
    assert_eq!(out, "NoOp", "a foreign keyed disabled marketplace is not ours, got {out}");
    assert_eq!(fs::read(&state_path).unwrap(), state_before, "self-heal must not mutate a foreign disabled keyed listing");
    assert!(!env.data.join(PLUGIN_NAME).join("markers").exists(), "self-heal must not write a marker for a foreign disabled keyed listing");
}

/// A keyless foreign listing under our marker: the user removed our marketplace
/// and re-added their own same-named one without re-installing. self-heal clears
/// our marker then no-ops, never resurrecting the listing.
#[test]
fn copilot_cli_self_heal_clears_our_marker_over_a_foreign_keyless_registration() {
    let env = Env::new("foreign-keyless-marker");
    let state_path = env.copilot_home.join("fake-copilot-state.json");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let seed = serde_json::json!({
        "plugins": [{"id": "ez-fixture-plugin@ez-fixture-plugin", "disabled": true}],
        "marketplaces": [{"name": "ez-fixture-plugin", "path": "/tmp/foreign"}],
    });
    fs::write(&state_path, serde_json::to_vec_pretty(&seed).unwrap()).unwrap();
    let state_before = fs::read(&state_path).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal must not fail over a keyless foreign listing: {out}");
    assert_eq!(out, "Cleared", "a marker over a keyless foreign listing clears, got {out}");
    assert_eq!(marker_count(&env), 0, "the marker must be cleared");
    assert_eq!(fs::read(&state_path).unwrap(), state_before, "self-heal must not mutate the foreign keyless listing");

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "the settling heal must not fail: {out}");
    assert_eq!(out, "NoOp", "a markerless heal over a keyless foreign listing no-ops, got {out}");
}

/// A keyless `(disabled)` row from our own legacy plain `current` registration
/// is ours, just served from a stale pointer: self-heal must fail loud with the
/// host-uninstall fix, never go silent.
#[test]
fn copilot_cli_self_heal_fails_loud_over_a_legacy_keyless_registration() {
    let env = Env::new("legacy-keyless");
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let registered = env.data.join(PLUGIN_NAME).join("current");

    let seed = serde_json::json!({
        "plugins": [{"id": "ez-fixture-plugin@ez-fixture-plugin", "disabled": true}],
        "marketplaces": [{"name": "ez-fixture-plugin", "path": registered}],
    });
    fs::write(&state_path, serde_json::to_vec_pretty(&seed).unwrap()).unwrap();

    let (ok, _out, err) = env.fixture_err(&["self-heal"]);
    assert!(!ok, "self-heal must fail loud over a legacy keyless registration");
    let ours = env.data.join(PLUGIN_NAME).join("current@copilot-cli");
    let want = format!(
        "error: copilot-cli: ez-fixture-plugin@ez-fixture-plugin is served from marketplace `ez-fixture-plugin` registered at `{}`, not this host's `{}`, so copilot never sees the re-staged tree; run the host binary's `uninstall` subcommand (it drops the legacy registration), then re-run setup",
        registered.display(),
        ours.display()
    );
    assert_eq!(err, want, "the error must name the legacy registration and the host-uninstall fix");
}

/// doctor never calls a foreign same-named registration ours: instead of an
/// `[ ok ] plugin registered` line, it names the foreign registration with the
/// `marketplace remove` fix.
#[test]
fn copilot_cli_doctor_names_a_foreign_registration_without_calling_it_ours() {
    let env = Env::new("doctor-foreign-registered");
    let state_path = env.copilot_home.join("fake-copilot-state.json");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let mut state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    state["marketplaces"][0]["path"] = serde_json::json!("/elsewhere");
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["doctor"]);
    assert!(!ok, "doctor must fail over a foreign registration");
    assert!(!out.lines().any(|l| l.starts_with("[ ok ] plugin registered")), "doctor must not call a foreign row ours, got:\n{out}");
    let ours = env.data.join(PLUGIN_NAME).join("current@copilot-cli");
    let want_fail = format!(
        "[fail] plugin registered: ez-fixture-plugin@ez-fixture-plugin is installed from a foreign marketplace: it is registered at `/elsewhere` but this host's is `{}`",
        ours.display()
    );
    let want_fix =
        "run `copilot plugin marketplace remove ez-fixture-plugin` (it uninstalls every plugin from that marketplace), then re-run setup";
    assert!(out.lines().any(|l| l == want_fail), "doctor must name the foreign registration, got:\n{out}");
    assert!(out.lines().any(|l| l == format!("       fix: {want_fix}")), "doctor must name the foreign fix, got:\n{out}");
}

/// A github source compares marketplace repos exactly: a same-named marketplace
/// at another repo is not ours, so self-heal no-ops (no enable, no marker) and
/// an explicit setup refuses it; the control repo converges as ours.
#[test]
fn copilot_cli_github_source_leaves_a_foreign_repo_marketplace_alone() {
    let env = Env::new("gh-foreign");
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    env.seed_settings();
    let settings_before = env.settings();
    let seed = |repo: &str| {
        serde_json::json!({
            "plugins": [{"id": "ez-fixture-plugin@ez-fixture-plugin", "disabled": true}],
            "marketplaces": [{"name": "ez-fixture-plugin", "repo": repo}],
        })
    };

    fs::write(&state_path, serde_json::to_vec_pretty(&seed("someone/fork")).unwrap()).unwrap();
    let state_before = fs::read(&state_path).unwrap();

    let (ok, out) = env.github_fixture(&["self-heal"]);
    assert!(ok, "self-heal must not fail over a foreign github repo: {out}");
    assert_eq!(out, "NoOp", "a foreign github repo is not ours, got {out}");
    assert_eq!(fs::read(&state_path).unwrap(), state_before, "self-heal must not enable or mark a foreign github repo");
    assert!(!env.data.join(PLUGIN_NAME).join("markers").exists(), "self-heal must not write a marker for a foreign github repo");

    let (ok, out) = env.github_fixture(&["setup-report"]);
    assert!(!ok, "an explicit setup over a foreign github repo must fail");
    let want_fail = "copilot-cli: failed: ez-fixture-plugin@ez-fixture-plugin is installed from marketplace `ez-fixture-plugin`, which is not this host's to manage: it is registered as `(GitHub: someone/fork)` but this host's is `uwuclxdy/agentgear`; run `copilot plugin marketplace remove ez-fixture-plugin` (it uninstalls every plugin from that marketplace), then re-run setup";
    assert!(out.lines().any(|l| l == want_fail), "setup must name the foreign github registration by equality, got:\n{out}");
    assert!(!env.data.join(PLUGIN_NAME).join("markers").exists(), "setup must not write a marker for a foreign github repo");
    assert_eq!(fs::read(&state_path).unwrap(), state_before, "setup must leave the foreign row untouched");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(state["plugins"][0]["disabled"], serde_json::json!(true), "setup must not enable the foreign row");
    assert_eq!(env.settings(), settings_before, "setup must not touch the user's settings");

    // Control: our own repo converges (the user's add-only listing of the exact repo).
    fs::write(&state_path, serde_json::to_vec_pretty(&seed("uwuclxdy/agentgear")).unwrap()).unwrap();
    let (ok, out) = env.github_fixture(&["self-heal"]);
    assert!(ok, "self-heal over our github repo must converge: {out}");
    assert_eq!(out, "Repaired", "our own repo's add-only listing converges, got {out}");
}

/// A same-named marketplace the user added but never installed (a `(disabled)`
/// row with no `enabledPlugins` key, registered elsewhere) is not ours: self-heal
/// must leave it alone — no error, no marker, no mutation — while an explicit
/// `setup` is the user asking for an install, so it still fails loud with the
/// foreign fix.
#[test]
fn copilot_cli_self_heal_leaves_a_foreign_keyless_marketplace_listing_alone() {
    let env = Env::new("foreign-keyless");
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    env.seed_settings();
    let settings_before = env.settings();

    // A hand `marketplace add` of a same-named marketplace that was never
    // installed: a `(disabled)` row, no `enabledPlugins` key, registered at a
    // path this host never added.
    let seed = serde_json::json!({
        "plugins": [{"id": "ez-fixture-plugin@ez-fixture-plugin", "disabled": true}],
        "marketplaces": [{"name": "ez-fixture-plugin", "path": "/tmp/foreign"}],
    });
    fs::write(&state_path, serde_json::to_vec_pretty(&seed).unwrap()).unwrap();
    let state_before = fs::read(&state_path).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal must not fail over a foreign keyless listing: {out}");
    assert_eq!(out, "NoOp", "a foreign keyless listing is not ours to converge, got {out}");
    assert_eq!(fs::read(&state_path).unwrap(), state_before, "self-heal must not mutate a foreign keyless listing");
    assert_eq!(env.settings(), settings_before, "self-heal must not touch the user's settings");
    assert!(!env.data.join(PLUGIN_NAME).join("markers").exists(), "self-heal must not write a marker for a foreign keyless listing");

    let (ok, _out, err) = env.fixture_err(&["setup", "--agent", "copilot-cli"]);
    assert!(!ok, "an explicit setup over a foreign keyless listing must fail");
    let ours = env.data.join(PLUGIN_NAME).join("current@copilot-cli");
    let want = format!(
        "error: copilot-cli: ez-fixture-plugin@ez-fixture-plugin is served from marketplace `ez-fixture-plugin` registered at `/tmp/foreign`, not this host's `{}`, so copilot never sees the re-staged tree; run `copilot plugin marketplace remove ez-fixture-plugin` (it uninstalls every plugin from that marketplace), then re-run setup",
        ours.display()
    );
    assert_eq!(err, want, "the setup error must name the foreign registration and fix");
    let doc: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(doc["plugins"][0]["disabled"], serde_json::json!(true), "the failed setup must not enable the row");
}

/// An explicit `setup` over a disabled row from a legacy registration must fail
/// the pointer check before enabling: the error leaves the row `(disabled)`, so
/// a failed setup never turns on an install copilot does not serve.
#[test]
fn copilot_cli_explicit_setup_over_a_disabled_legacy_registration_errors_without_enabling() {
    let env = Env::new("disabled-legacy-setup");
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let read_state = || serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&state_path).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["plugin", "disable", "ez-fixture-plugin@ez-fixture-plugin"]);
    env.apply(&mut cmd);
    assert!(cmd.output().unwrap().status.success(), "the fake disable failed");

    // Re-register our marketplace at the pre-scoping plain `current`.
    let mut state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let registered = env.data.join(PLUGIN_NAME).join("current");
    state["marketplaces"][0]["path"] = serde_json::json!(registered);
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let (ok, _out, err) = env.fixture_err(&["setup", "--agent", "copilot-cli"]);
    assert!(!ok, "setup must fail loud when our marketplace is not at our pointer");
    let ours = env.data.join(PLUGIN_NAME).join("current@copilot-cli");
    let want = format!(
        "error: copilot-cli: ez-fixture-plugin@ez-fixture-plugin is served from marketplace `ez-fixture-plugin` registered at `{}`, not this host's `{}`, so copilot never sees the re-staged tree; run the host binary's `uninstall` subcommand (it drops the legacy registration), then re-run setup",
        registered.display(),
        ours.display()
    );
    assert_eq!(err, want, "the error must name the registration, our pointer, and the fix");

    let doc = read_state();
    assert_eq!(doc["plugins"][0]["disabled"], serde_json::json!(true), "the failed setup must not enable the row:\n{doc}");
}

/// doctor compares the marketplace registration with our pointer, so a legacy
/// or foreign registration that makes every heal error fails the report with
/// the same fix hint instead of reading all-green.
#[test]
fn copilot_cli_doctor_names_a_registration_served_from_elsewhere() {
    let env = Env::new("doctor-foreign-registration");
    let state_path = env.copilot_home.join("fake-copilot-state.json");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let mut state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let registered = env.data.join(PLUGIN_NAME).join("current");
    state["marketplaces"][0]["path"] = serde_json::json!(registered);
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["doctor"]);
    assert!(!ok, "doctor must fail over a registration served from elsewhere");
    let ours = env.data.join(PLUGIN_NAME).join("current@copilot-cli");
    let want_fail = format!(
        "[fail] marketplace served from pointer: ez-fixture-plugin@ez-fixture-plugin is served from marketplace `ez-fixture-plugin` registered at `{}`, not this host's `{}`, so copilot never sees the re-staged tree",
        registered.display(),
        ours.display()
    );
    let want_fix = "run the host binary's `uninstall` subcommand (it drops the legacy registration), then re-run setup";
    assert!(out.lines().any(|l| l == want_fail), "doctor must fail the pointer check with the same problem, got:\n{out}");
    assert!(out.lines().any(|l| l == format!("       fix: {want_fix}")), "doctor must name the same fix hint, got:\n{out}");
}

/// A local foreign registration is named by one `[fail]` line — the `plugin
/// registered` check, with the `marketplace remove` fix — and the pointer check
/// must not add a second failure for the same registration.
#[test]
fn copilot_cli_doctor_names_a_foreign_registration_once() {
    let env = Env::new("doctor-foreign-once");
    let state_path = env.copilot_home.join("fake-copilot-state.json");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let mut state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    state["marketplaces"][0]["path"] = serde_json::json!("/elsewhere");
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["doctor"]);
    assert!(!ok, "doctor must fail over a foreign registration");
    let ours = env.data.join(PLUGIN_NAME).join("current@copilot-cli");
    let want_fail = format!(
        "[fail] plugin registered: ez-fixture-plugin@ez-fixture-plugin is installed from a foreign marketplace: it is registered at `/elsewhere` but this host's is `{}`",
        ours.display()
    );
    assert!(out.lines().any(|l| l == want_fail), "doctor must name the foreign registration by equality, got:\n{out}");
    assert!(
        !out.lines().any(|l| l.contains("marketplace served from pointer")),
        "a foreign registration must not also fail the pointer check, got:\n{out}"
    );
}

/// doctor pins the healthy pointer check by equality, so a good registration's
/// `[ ok ]` line is covered, not just the failing ones.
#[test]
fn copilot_cli_doctor_names_the_served_from_pointer_ok() {
    let env = Env::new("doctor-pointer-ok");
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let (ok, out) = env.fixture(&["doctor"]);
    assert!(ok, "doctor failed over a healthy install: {out}");
    let ours = env.data.join(PLUGIN_NAME).join("current@copilot-cli");
    let want = format!("[ ok ] marketplace served from pointer: `ez-fixture-plugin` registered at `{}`", ours.display());
    assert!(out.lines().any(|l| l == want), "doctor must pin the pointer-ok line, got:\n{out}");
}

// --- instructions ------------------------------------------------------------

/// The fixture's `<name>@<marketplace>` plugin id; the derive defaults `marketplace`
/// to `name` when no attr names it.
const INSTR_ID: &str = "ez-fixture-plugin@ez-fixture-plugin";
const INSTR_GUIDANCE: &str = "ez-fixture always-loaded guidance line.\n";

/// The exact tagged block the instructions surface must render into
/// `$COPILOT_HOME/copilot-instructions.md`: the host guidance between two html-comment
/// markers naming the plugin id, the whole block replaceable byte-for-byte.
fn instr_block() -> String {
    format!("<!-- agentgear:{INSTR_ID} begin -->\n{INSTR_GUIDANCE}<!-- agentgear:{INSTR_ID} end -->\n")
}

/// The user-scope instructions lifecycle in copilot's `copilot-instructions.md` (the
/// CLI lists it as a `home` custom-instruction file and ships it in the system prompt
/// with html comments intact): install appends our block after the user's content with
/// one blank separator line; a drifted or deleted block is repaired by self-heal;
/// uninstall strips exactly our block and the separator.
#[test]
fn copilot_cli_instructions_block_lifecycle() {
    let env = Env::new("instr-lifecycle");
    let instructions = env.copilot_home.join("copilot-instructions.md");
    let seed = "user instruction one\nuser instruction two\n";
    fs::write(&instructions, seed).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");
    let installed = format!("{seed}\n{}", instr_block());
    assert_eq!(fs::read_to_string(&instructions).unwrap(), installed, "install must append exactly our block after a blank line");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");
    assert_eq!(fs::read_to_string(&instructions).unwrap(), installed, "a no-op setup must not rewrite copilot-instructions.md");

    let tampered = format!("{seed}\n<!-- agentgear:{INSTR_ID} begin -->\ntampered\n<!-- agentgear:{INSTR_ID} end -->\n");
    fs::write(&instructions, &tampered).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the drifted block: {out}");
    assert_eq!(fs::read_to_string(&instructions).unwrap(), installed, "self-heal must restore the exact block bytes");

    fs::write(&instructions, seed).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the deleted block: {out}");
    assert_eq!(fs::read_to_string(&instructions).unwrap(), installed, "self-heal must re-add the block");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(fs::read_to_string(&instructions).unwrap(), seed, "uninstall must restore the user's file byte-for-byte");
}

/// With no pre-existing `copilot-instructions.md`, install creates the file holding
/// only our block, and uninstall — our removal being what emptied it — takes the file
/// with it.
#[test]
fn copilot_cli_instructions_owns_only_its_file() {
    let env = Env::new("instr-owned-file");
    let instructions = env.copilot_home.join("copilot-instructions.md");
    assert!(!instructions.exists(), "precondition: no copilot-instructions.md seeded");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(
        fs::read_to_string(&instructions).unwrap(),
        instr_block(),
        "a file we created holds exactly our block, no leading blank line"
    );

    // The Ok line pins the `instructions.is_some()` gate, the push and the user-scope path it is handed.
    let (ok, out) = env.fixture(&["doctor"]);
    assert!(ok, "doctor failed over a healthy install: {out}");
    let want = format!("[ ok ] guidance block present: block present in {}", instructions.display());
    assert!(out.lines().any(|l| l == want), "the doctor report lacks an ok guidance-block check over a healthy install:\n{out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert!(!instructions.exists(), "uninstall must remove the file our removal emptied");
}

/// A rival agentgear host's block (different plugin id in the markers) and the user's
/// own lines must survive our install and uninstall byte-identical: the marker id
/// scopes ownership, so two agentgear plugins coexist in one instructions file.
#[test]
fn copilot_cli_never_clobbers_a_rival_instructions_block() {
    let env = Env::new("instr-rival");
    let instructions = env.copilot_home.join("copilot-instructions.md");
    let rival =
        "user line\n<!-- agentgear:rival-plugin@rival-mkt begin -->\nrival guidance\n<!-- agentgear:rival-plugin@rival-mkt end -->\n";
    fs::write(&instructions, rival).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(
        fs::read_to_string(&instructions).unwrap(),
        format!("{rival}\n{}", instr_block()),
        "install must append our block, leaving the rival's untouched"
    );

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(
        fs::read_to_string(&instructions).unwrap(),
        rival,
        "uninstall must strip only our block, keeping the rival's and the user's"
    );
}

/// copilot also loads the modular `instructions/**/*.instructions.md` tree, but that is
/// the user's own layout — we own one block in the single home file, never a file the
/// user may be hand-managing. Install must leave that tree unwritten.
#[test]
fn copilot_cli_instructions_leaves_the_modular_tree_unwritten() {
    let env = Env::new("instr-modular");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert!(env.copilot_home.join("copilot-instructions.md").exists(), "precondition: our block landed");

    let modular = env.copilot_home.join("instructions");
    assert!(!modular.exists(), "the modular instructions tree is the user's, never ours to create: {}", modular.display());
}

// --- teardown guards ---------------------------------------------------------

#[test]
fn copilot_cli_uninstall_clears_our_marker_over_a_foreign_same_named_marketplace() {
    let env = Env::new("foreign-mkt");
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // Re-point our marketplace's name at a path this host never added (the user's
    // own same-named marketplace). Uninstall must drop only what is ours
    // (guidance + marker), never the user's own registration or the plugin
    // installed from it.
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let mut state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    state["marketplaces"][0]["path"] = serde_json::json!("/elsewhere");
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall must converge over a foreign marketplace, got: {out}");
    assert_eq!(out, "Removed", "uninstall strips our guidance, got {out}");
    assert_eq!(marker_count(&env), 0, "uninstall must clear our marker");

    let state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(state["marketplaces"][0]["path"], serde_json::json!("/elsewhere"), "the foreign marketplace must survive:\n{state}");
    assert_eq!(state["plugins"][0]["disabled"], serde_json::json!(false), "uninstall must not disable the user's own install:\n{state}");
}

#[test]
fn copilot_cli_uninstall_keeps_a_shared_marketplace_and_errors() {
    let env = Env::new("shared-mkt");
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // Seed a sibling plugin from our marketplace: `marketplace remove` would drop
    // it too (>= 1.0.81), so the backend must leave the marketplace registered.
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let mut state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    state["plugins"].as_array_mut().unwrap().push(serde_json::json!({"id": "other-plugin@ez-fixture-plugin", "disabled": false}));
    state["enabledPlugins"]["other-plugin@ez-fixture-plugin"] = serde_json::json!(true);
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let (ok, _out, err) = env.fixture_err(&["uninstall"]);
    assert!(!ok, "uninstall must fail when our marketplace is shared");
    let want = "error: copilot-cli: ez-fixture-plugin@ez-fixture-plugin was uninstalled but marketplace `ez-fixture-plugin` stays because `other-plugin@ez-fixture-plugin` still uses it; `copilot plugin marketplace remove` would uninstall it too";
    assert_eq!(err, want, "the shared-teardown error must name the sibling and never suggest dropping it");

    let state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let sibling_survives =
        state["plugins"].as_array().is_some_and(|ps| ps.iter().any(|p| p["id"] == serde_json::json!("other-plugin@ez-fixture-plugin")));
    assert!(sibling_survives, "the sibling plugin must survive:\n{state}");
    let marketplace_survives =
        state["marketplaces"].as_array().is_some_and(|ms| ms.iter().any(|m| m["name"] == serde_json::json!("ez-fixture-plugin")));
    assert!(marketplace_survives, "the shared marketplace must survive:\n{state}");

    // A kept (shared) marketplace leaves our row listed `(disabled)`; its `false`
    // key is copilot's record of that disable, not a stale leftover — the teardown
    // must keep it, or a later markerless self-heal reads a keyless half-install
    // and re-enables the plugin.
    assert_eq!(
        state["enabledPlugins"]["ez-fixture-plugin@ez-fixture-plugin"],
        serde_json::json!(false),
        "a kept row must keep copilot's disable record:\n{state}"
    );

    // A markerless self-heal over that kept row must still read a deliberate
    // disable, never converge it back to enabled.
    fs::remove_dir_all(env.data.join(PLUGIN_NAME).join("markers")).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");
    assert_eq!(out, "NoOp", "a markerless self-heal must not re-enable a kept-row disable, got {out}");
    let state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(state["plugins"][0]["disabled"], serde_json::json!(true), "the kept row must stay disabled:\n{state}");
}

/// A sibling from our marketplace that is merely listed (no `enabledPlugins`
/// key) is not an install, so `uninstall` must drop the marketplace rather than
/// treating it as shared and erroring.
#[test]
fn copilot_cli_uninstall_drops_a_marketplace_whose_sibling_is_only_listed() {
    let env = Env::new("listed-sibling");
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let state_path = env.copilot_home.join("fake-copilot-state.json");
    let mut state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    state["plugins"].as_array_mut().unwrap().push(serde_json::json!({"id": "other-plugin@ez-fixture-plugin", "disabled": true}));
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(out, "Removed", "a keyless sibling must not keep the marketplace, got {out}");
    let state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(state["marketplaces"], serde_json::json!([]), "a merely-listed sibling must not keep the marketplace:\n{state}");
}

#[test]
fn fake_copilot_config_rm_rejects_a_dotted_key() {
    // The real CLI errors on a dotted id (`Cannot descend past record leaf`), so
    // the double must too; the backend's dotted-key skip relies on this parity.
    let env = Env::new("dotted-config-rm");
    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["config", "--rm", "enabledPlugins.p@a.b"]);
    env.apply(&mut cmd);
    let out = cmd.output().unwrap();
    assert!(!out.status.success(), "fake_copilot must reject a dotted config key");
}

#[test]
fn fake_copilot_config_enabledplugins_reads_an_empty_group_like_real_1_0_104() {
    // Real 1.0.104 reads a present-but-empty `enabledPlugins: {}` as rc 0 with `{}`,
    // and an absent key as rc 1 with no output (measured); the double models both.
    let env = Env::new("empty-enabled-group");
    let state_path = env.copilot_home.join("fake-copilot-state.json");
    fs::write(&state_path, serde_json::to_vec_pretty(&serde_json::json!({"enabledPlugins": {}})).unwrap()).unwrap();

    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["config", "enabledPlugins"]);
    env.apply(&mut cmd);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "an empty enabledPlugins group must read rc 0");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{}");
}

/// The fake models real 1.0.104's `marketplace add`: it writes no `enabledPlugins`
/// key and lists every plugin of the local marketplace as a `(disabled)` row before
/// any `plugin install` (measured).
#[test]
fn fake_copilot_marketplace_add_writes_no_enabledplugins_key_and_lists_plugins_disabled() {
    let env = Env::new("mkt-add-disabled");
    let mkt = env.root.join("mkt");
    fs::create_dir_all(mkt.join(".claude-plugin")).unwrap();
    fs::write(
        mkt.join(".claude-plugin").join("marketplace.json"),
        r#"{"name":"m1","owner":{"name":"x"},"plugins":[{"name":"p1","source":"./p1","description":"a"}]}"#,
    )
    .unwrap();

    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["plugin", "marketplace", "add", mkt.to_str().unwrap()]);
    env.apply(&mut cmd);
    assert!(cmd.output().unwrap().status.success(), "the fake marketplace add failed");

    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(env.copilot_home.join("fake-copilot-state.json")).unwrap()).unwrap();
    assert!(state.get("enabledPlugins").is_none(), "marketplace add must not write an enabledPlugins key:\n{state}");
    assert_eq!(state["plugins"][0]["id"], serde_json::json!("p1@m1"), "the marketplace's plugin must be listed:\n{state}");
    assert_eq!(
        state["plugins"][0]["disabled"],
        serde_json::json!(true),
        "the marketplace's plugin must list as (disabled) before install:\n{state}"
    );

    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["plugin", "list"]);
    env.apply(&mut cmd);
    let out = cmd.output().unwrap();
    assert!(out.status.success());
    let list = String::from_utf8_lossy(&out.stdout);
    assert!(list.contains("p1@m1 (disabled)"), "plugin list must show the (disabled) row:\n{list}");
}

/// The fake models real copilot's refusal to re-point an existing marketplace name:
/// `marketplace add` exits 1 with `Marketplace "…" already registered` (measured).
#[test]
fn fake_copilot_marketplace_add_refuses_an_existing_name() {
    let env = Env::new("mkt-add-refuses");
    let mkt = env.root.join("mkt");
    fs::create_dir_all(mkt.join(".claude-plugin")).unwrap();
    fs::write(
        mkt.join(".claude-plugin").join("marketplace.json"),
        r#"{"name":"m1","owner":{"name":"x"},"plugins":[{"name":"p1","source":"./p1","description":"a"}]}"#,
    )
    .unwrap();

    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["plugin", "marketplace", "add", mkt.to_str().unwrap()]);
    env.apply(&mut cmd);
    assert!(cmd.output().unwrap().status.success(), "the first add must succeed");

    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["plugin", "marketplace", "add", mkt.to_str().unwrap()]);
    env.apply(&mut cmd);
    let out = cmd.output().unwrap();
    assert!(!out.status.success(), "a re-add of an existing name must fail");
    assert!(String::from_utf8_lossy(&out.stderr).contains("already registered"), "the refusal must name the existing marketplace");
}

/// The backend's post-enable re-read: when `plugin enable` exits 0 without flipping
/// the row (real copilot's "no plugin named ..." warning), an explicit setup fails
/// loud instead of reporting `Repaired` over a still-disabled row.
#[test]
fn copilot_cli_explicit_setup_fails_loud_when_enable_does_not_flip_the_row() {
    let env = Env::new("enable-refused");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let mut cmd = Command::new(FAKE_COPILOT);
    cmd.args(["plugin", "disable", "ez-fixture-plugin@ez-fixture-plugin"]);
    env.apply(&mut cmd);
    assert!(cmd.output().unwrap().status.success(), "the fake disable failed");

    let (ok, _out, err) = env.fixture_err_with(&["setup", "--agent", "copilot-cli"], &[("FAKE_COPILOT_REFUSE_ENABLE", "1")]);
    assert!(!ok, "setup must fail when the enable does not flip the row");
    assert_eq!(
        err,
        "error: copilot-cli: post-op verification failed: ez-fixture-plugin@ez-fixture-plugin still disabled after `copilot plugin enable`"
    );
}

/// An explicit `setup` over an enabled foreign row at our own version — the
/// `NoOp` arm, with the marker's tree hash still matching — must refuse before
/// that arm can no-op and stamp ownership over the user's row.
#[test]
fn copilot_cli_setup_refuses_a_foreign_enabled_registration_on_the_noop_arm() {
    let env = Env::new("foreign-noop-setup");
    let state_path = env.copilot_home.join("fake-copilot-state.json");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // Re-point our marketplace name at a path this host never added, leaving the
    // row enabled at our version and the marker's tree hash matching.
    let mut state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    state["marketplaces"][0]["path"] = serde_json::json!("/elsewhere");
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    let state_before = fs::read(&state_path).unwrap();
    let markers_before = marker_count(&env);

    let (ok, _out, err) = env.fixture_err(&["setup", "--agent", "copilot-cli"]);
    assert!(!ok, "an explicit setup over a foreign enabled registration must fail");
    let ours = env.data.join(PLUGIN_NAME).join("current@copilot-cli");
    let want = format!(
        "error: copilot-cli: ez-fixture-plugin@ez-fixture-plugin is served from marketplace `ez-fixture-plugin` registered at `/elsewhere`, not this host's `{}`, so copilot never sees the re-staged tree; run `copilot plugin marketplace remove ez-fixture-plugin` (it uninstalls every plugin from that marketplace), then re-run setup",
        ours.display()
    );
    assert_eq!(err, want, "the setup error must name the foreign registration and fix");
    assert_eq!(marker_count(&env), markers_before, "the failed setup must not write or clear the marker");
    assert_eq!(fs::read(&state_path).unwrap(), state_before, "the failed setup must leave the row untouched");
}

/// An explicit `setup` over an enabled foreign row at a strictly-newer version —
/// the `Frozen` arm, markerless — must refuse before that arm can no-op and stamp
/// ownership over the user's row.
#[test]
fn copilot_cli_setup_refuses_a_foreign_newer_enabled_registration_on_the_frozen_arm() {
    let env = Env::new("foreign-frozen-setup");
    let state_path = env.copilot_home.join("fake-copilot-state.json");

    let seed = serde_json::json!({
        "plugins": [{"id": "ez-fixture-plugin@ez-fixture-plugin", "disabled": false, "version": "99.0.0"}],
        "marketplaces": [{"name": "ez-fixture-plugin", "path": "/tmp/foreign"}],
        "enabledPlugins": {"ez-fixture-plugin@ez-fixture-plugin": true},
    });
    fs::write(&state_path, serde_json::to_vec_pretty(&seed).unwrap()).unwrap();
    let state_before = fs::read(&state_path).unwrap();

    let (ok, _out, err) = env.fixture_err(&["setup", "--agent", "copilot-cli"]);
    assert!(!ok, "an explicit setup over a foreign newer registration must fail");
    let ours = env.data.join(PLUGIN_NAME).join("current@copilot-cli");
    let want = format!(
        "error: copilot-cli: ez-fixture-plugin@ez-fixture-plugin is served from marketplace `ez-fixture-plugin` registered at `/tmp/foreign`, not this host's `{}`, so copilot never sees the re-staged tree; run `copilot plugin marketplace remove ez-fixture-plugin` (it uninstalls every plugin from that marketplace), then re-run setup",
        ours.display()
    );
    assert_eq!(err, want, "the setup error must name the foreign registration and fix");
    assert!(!env.data.join(PLUGIN_NAME).join("markers").exists(), "the failed setup must not write a marker");
    assert_eq!(fs::read(&state_path).unwrap(), state_before, "the failed setup must leave the row untouched");
}

/// A healthy install's probe reads the row and the marketplace ownership alone:
/// computing the sibling set (`config enabledPlugins` + a second `plugin list`)
/// is uninstall's job, never a session-start probe's.
#[test]
fn copilot_cli_probe_reads_ownership_without_computing_siblings() {
    let env = Env::new("probe-spawns");
    let calls_log = env.copilot_home.join(FAKE_COPILOT_CALL_LOG);

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    fs::write(&calls_log, "").unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal must no-op over a healthy install, got {out}");
    let calls = fs::read_to_string(&calls_log).unwrap_or_default();
    assert_eq!(calls, "plugin list\nplugin marketplace list\n", "probe must spawn once for the row and once for ownership, got:\n{calls}");
}
