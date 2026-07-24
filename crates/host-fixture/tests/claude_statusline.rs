//! Hermetic Claude-backend `statusLine` lifecycle, fully isolated from a real
//! `~/.claude`. No docker, no auth, no real `claude`: the `fake_claude` bin is
//! copied onto a scratch `PATH` as `claude`, so the backend's CLI orchestration runs
//! against a throwaway registry while the part under test — the `statusLine` slot in
//! `$CLAUDE_CONFIG_DIR/settings.json` and the stamp-marker stash behind it — is real
//! code writing real files.
//!
//! The slot is single-valued and last-writer-wins, so the contract these tests pin is
//! stash-and-restore: install displaces whatever the user had into the marker, every
//! later `update`/`self-heal` must carry that stash forward untouched, and uninstall
//! puts the user's own value back byte-for-byte — but only while the live value is
//! still ours to take back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");
const FAKE_CLAUDE: &str = env!("CARGO_BIN_EXE_fake_claude");

/// The user's own settings before we touch anything: an unrelated key plus a real
/// status line of theirs. Written in `serde_json::to_vec_pretty` + trailing-newline
/// form — exactly what `confedit`'s writer emits — so uninstall must restore this
/// file byte-for-byte, not merely an equal value.
///
/// `echo their-bar-row` is deliberately runnable under both `sh -c` and `cmd /C`, so
/// the compose assertion below exercises the real subprocess path on every platform.
const SEED_SETTINGS: &str = r#"{
  "theirSetting": true,
  "statusLine": {
    "type": "command",
    "command": "echo their-bar-row",
    "padding": 2
  }
}
"#;

/// A session payload shaped like Claude Code's: `compose` reads `cwd` off it to pick
/// project-then-user scope, and pipes the whole thing to the user's own command.
const SESSION_JSON: &str = r#"{"session_id":"abc","cwd":"/nonexistent/project"}"#;

/// What the fixture host declares, with `${AGENTGEAR_CLIENT}` expanded for claude.
const OUR_COMMAND: &str = "host_fixture statusline --client claude";

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

    /// Run the fixture with `stdin` piped in — the status-line entrypoint's real
    /// calling convention.
    ///
    /// This one call gets the inherited `PATH` appended, because composing runs the
    /// user's own status command through the platform shell and the curated `PATH`
    /// above carries no `sh`/`cmd`. Safe to widen here and nowhere else: the
    /// status-line entrypoint never detects or writes a backend, so a stray harness
    /// CLI on the dev box cannot reach it.
    fn fixture_stdin(&self, args: &[&str], stdin: &str) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        self.apply(&mut cmd);
        cmd.env("PATH", with_inherited_path(&self.path));
        let mut child = cmd.spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim_end_matches(['\n', '\r']).to_string())
    }

    fn settings_path(&self) -> PathBuf {
        self.cfg.join("settings.json")
    }

    fn settings(&self) -> String {
        fs::read_to_string(self.settings_path()).unwrap()
    }

    fn status_line(&self) -> serde_json::Value {
        let parsed: serde_json::Value = serde_json::from_str(&self.settings()).unwrap();
        parsed.get("statusLine").cloned().unwrap_or(serde_json::Value::Null)
    }

    /// Overwrite the slot with someone else's value, as a second tool (or the user)
    /// would.
    fn set_status_line(&self, value: serde_json::Value) {
        let mut parsed: serde_json::Value = serde_json::from_str(&self.settings()).unwrap();
        parsed["statusLine"] = value;
        fs::write(self.settings_path(), serde_json::to_vec_pretty(&parsed).unwrap()).unwrap();
    }

    fn remove_status_line(&self) {
        let mut parsed: serde_json::Value = serde_json::from_str(&self.settings()).unwrap();
        parsed.as_object_mut().unwrap().remove("statusLine");
        fs::write(self.settings_path(), serde_json::to_vec_pretty(&parsed).unwrap()).unwrap();
    }

    /// The stash the claude backend recorded, as raw JSON.
    fn stashed_original(&self) -> serde_json::Value {
        let markers = self.data.join("ez-fixture-plugin").join("markers");
        let entries: Vec<_> = fs::read_dir(&markers).map(|d| d.flatten().collect()).unwrap_or_default();
        for entry in entries {
            let marker: serde_json::Value =
                serde_json::from_slice(&fs::read(entry.path()).unwrap_or_default()).unwrap_or(serde_json::Value::Null);
            if marker.get("agent").and_then(serde_json::Value::as_str) == Some("claude") {
                return marker.get("statusline_original").cloned().unwrap_or(serde_json::Value::Null);
            }
        }
        serde_json::Value::Null
    }

    /// Take the `claude` double off the scratch PATH — the user uninstalling Claude
    /// Code itself, which makes every claude-backend row a `NotDetected` skip.
    fn remove_claude_double(&self) {
        let binary = self.root.join("bin").join(format!("claude{}", std::env::consts::EXE_SUFFIX));
        fs::remove_file(&binary).unwrap();
    }

    /// Drop every plugin from the `fake_claude` registry, leaving the marketplace —
    /// what a hand-run `claude plugin uninstall` leaves behind.
    fn drop_plugin_from_registry(&self) {
        let path = self.cfg.join("fake-claude-state.json");
        let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        state["plugins"] = serde_json::json!([]);
        fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
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

/// `curated` first (so the scratch `claude` still wins), then whatever the test
/// process inherited.
fn with_inherited_path(curated: &OsString) -> OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(curated).chain(std::env::split_paths(&inherited)).collect();
    std::env::join_paths(&dirs).unwrap_or_else(|_| curated.clone())
}

fn seed_status_line() -> serde_json::Value {
    let parsed: serde_json::Value = serde_json::from_str(SEED_SETTINGS).unwrap();
    parsed.get("statusLine").cloned().unwrap()
}

/// The value the backend must have written: CC's single-object command shape with
/// `${AGENTGEAR_CLIENT}` already expanded.
fn our_status_line() -> serde_json::Value {
    serde_json::json!({"type": "command", "command": OUR_COMMAND, "padding": 0})
}

#[test]
fn claude_statusline_full_lifecycle() {
    let env = Env::new("lifecycle");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // Our declaration owns the slot, `${AGENTGEAR_CLIENT}` expanded to this backend.
    assert_eq!(env.status_line(), our_status_line(), "our statusLine did not land:\n{}", env.settings());
    // The user's unrelated key survived the read-modify-write.
    assert!(env.settings().contains("theirSetting"), "seeded top-level key was clobbered:\n{}", env.settings());

    // The compose helper runs the displaced command and appends its rows under ours.
    let (ok, out) = env.fixture_stdin(&["statusline", "--client", "claude"], SESSION_JSON);
    assert!(ok, "statusline subcommand failed: {out}");
    assert_eq!(out, "ez-fixture row\ntheir-bar-row", "compose did not stack our row over the user's");

    // Idempotent: a second identical reconcile writes nothing.
    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    // The user's own status line is back, byte for byte, and so is the whole file.
    assert_eq!(env.status_line(), seed_status_line(), "uninstall did not restore the user's statusLine");
    assert_eq!(env.settings(), SEED_SETTINGS, "uninstall did not restore settings.json byte-for-byte");
}

#[test]
fn claude_statusline_stash_survives_update_and_self_heal() {
    // The mutation this pins: `stamp::write` rebuilds the marker from scratch on
    // every install/update/self-heal, so without an explicit carry-forward the
    // displaced original is erased by the first `update` and uninstall silently
    // deletes the user's status line instead of restoring it.
    let env = Env::new("stash-survives");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let (ok, out) = env.fixture(&["update"]);
    assert!(ok, "update failed: {out}");
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");

    // Still ours between the two, so the restore below is a real restore.
    assert_eq!(env.status_line(), our_status_line(), "update/self-heal disturbed our statusLine");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(env.status_line(), seed_status_line(), "the stash did not survive update + self-heal");
    assert_eq!(env.settings(), SEED_SETTINGS, "uninstall did not restore settings.json byte-for-byte");
}

#[test]
fn claude_statusline_drift_is_repaired_without_losing_the_stash() {
    // A user who deletes our line leaves the registry healthy and the slot empty:
    // probe must read that as drift, reconcile must re-add ours, and the earlier
    // stash must NOT be overwritten by the now-empty slot.
    let env = Env::new("drift");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    env.remove_status_line();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored on a drifted statusLine: {out}");
    assert_eq!(out, "Repaired", "a missing statusLine behind a healthy registry is drift, got {out}");
    assert_eq!(env.status_line(), our_status_line(), "self-heal did not re-add our statusLine");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(env.status_line(), seed_status_line(), "the repair pass overwrote the user's stashed original");
}

#[test]
fn claude_statusline_our_own_earlier_rendering_is_never_stashed() {
    // The failure this pins: ownership decided by whole-value equality reads our OWN
    // previous rendering as "the user's original" the moment a host release changes
    // its padding or its flags. That destroys the user's value AND poisons the stash
    // with our own command, which `compose` would then run from inside itself, on
    // every turn the harness re-renders.
    let env = Env::new("own-rendering");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert_eq!(env.stashed_original(), seed_status_line(), "install did not stash the user's original");

    // Exactly what a prior host version would have left in the slot: our command,
    // different padding.
    env.set_status_line(serde_json::json!({"type": "command", "command": OUR_COMMAND, "padding": 1}));

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored on our own drifted rendering: {out}");
    assert_eq!(out, "Repaired", "our own drifted rendering is drift, got {out}");
    assert_eq!(env.status_line(), our_status_line(), "self-heal did not converge our own rendering");

    // Asserted BEFORE the stash below on purpose: this is the anti-recursion guard's
    // positive control. Break the ownership test and the stash holds OUR command, so
    // this call is what would re-enter the binary instead of returning a row.
    let (ok, out) = env.fixture_stdin(&["statusline", "--client", "claude"], SESSION_JSON);
    assert!(ok, "statusline subcommand failed: {out}");
    assert_eq!(out, "ez-fixture row\ntheir-bar-row", "compose lost the user's row");

    assert_eq!(env.stashed_original(), seed_status_line(), "our own earlier rendering was stashed as the user's original");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(env.status_line(), seed_status_line(), "uninstall left a command pointing at the uninstalled binary");
}

#[test]
fn claude_statusline_manual_plugin_uninstall_restores_the_user_line() {
    // self_heal's "plugin already gone under our marker" row clears the marker, and
    // the stash goes with it. Without a restore first, the user is left with our
    // command pointing at an uninstalled plugin and no copy of their own value
    // anywhere on disk.
    let env = Env::new("manual-uninstall");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    env.drop_plugin_from_registry();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored after a manual plugin uninstall: {out}");
    assert_eq!(out, "Cleared", "a hand-removed plugin under our marker should clear, got {out}");
    assert_eq!(env.status_line(), seed_status_line(), "the user's status line was not restored before the stash was dropped");
    assert_eq!(env.settings(), SEED_SETTINGS, "settings.json was not restored byte-for-byte");

    // Never resurrect: the plugin stays gone.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "a second heal must stay out, got {out}");
}

#[test]
fn claude_statusline_uninstall_restores_when_the_harness_itself_is_gone() {
    // `uninstall`'s skip rows never call `remove`, so a user who uninstalled Claude
    // Code before running our uninstall would keep our command in settings.json while
    // the marker holding their original is cleared out from under it. The slot lives
    // in the user's own settings file, which resolves with no `claude` on PATH at all.
    let env = Env::new("harness-gone");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert_eq!(env.stashed_original(), seed_status_line(), "install did not stash the user's original");

    env.remove_claude_double();

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall errored with the harness gone: {out}");
    assert_eq!(out, "NoOp", "every agent should skip with nothing detected, got {out}");
    assert_eq!(env.status_line(), seed_status_line(), "a skipped uninstall stranded our command and dropped the stash");
    assert_eq!(env.settings(), SEED_SETTINGS, "settings.json was not restored byte-for-byte");
}

#[test]
fn claude_statusline_remove_leaves_a_foreign_value_alone() {
    // Exact-remove: once someone else owns the slot, uninstall must not touch it —
    // not even to restore what we stashed, which is no longer what the user sees.
    let env = Env::new("foreign");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let foreign = serde_json::json!({"type": "command", "command": "someone-elses-bar"});
    env.set_status_line(foreign.clone());

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(env.status_line(), foreign, "uninstall clobbered a statusLine that was no longer ours");
}
