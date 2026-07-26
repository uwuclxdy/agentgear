//! Hermetic copilot-cli `statusLine` lifecycle, fully isolated from a real
//! `~/.copilot`. No docker, no auth, no real `copilot`: the `fake_copilot` bin is
//! copied onto a scratch `PATH` as `copilot`, so the backend's CLI orchestration runs
//! against a throwaway registry while the part under test — the `statusLine` slot in
//! `$COPILOT_HOME/settings.json` and the stamp-marker stash behind it — is real code
//! writing real files.
//!
//! The slot is single-valued and last-writer-wins, so the contract these tests pin is
//! stash-and-restore: install displaces whatever the user had into the marker, every
//! later `update`/`self-heal` must carry that stash forward untouched, and uninstall
//! puts the user's own value back byte-for-byte — but only while the live value is
//! still ours to take back.
//!
//! copilot's slot is USER SCOPE ONLY (its repo-scope override list is built inside
//! copilot's Rust `runtime.node` and could not be enumerated), so a project-scope pass
//! must write nothing here and say so in its report.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");
const FAKE_COPILOT: &str = env!("CARGO_BIN_EXE_fake_copilot");

/// Mirrors `fake_copilot.rs`'s own state-file name; a bin target exports nothing an
/// external test crate can import, so this is a second literal by necessity, not by
/// oversight (same idiom `claude_statusline.rs` uses for `fake_claude`'s).
const FAKE_COPILOT_STATE_FILENAME: &str = "fake-copilot-state.json";

/// The user's own settings before we touch anything: an unrelated key plus a real
/// status line of theirs. Written in `serde_json::to_vec_pretty` + trailing-newline
/// form — exactly what `confedit`'s writer emits — so uninstall must restore this file
/// byte-for-byte, not merely an equal value.
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

/// A session payload shaped like a harness's: `compose` reads `cwd` off it to pick
/// project-then-user scope, and pipes the whole thing to the user's own command.
const SESSION_JSON: &str = r#"{"session_id":"abc","cwd":"/nonexistent/project"}"#;

/// What the fixture host declares, with `${AGENTGEAR_CLIENT}` expanded for this backend.
const OUR_COMMAND: &str = "host_fixture statusline --client copilot-cli";

struct Env {
    root: PathBuf,
    /// `$COPILOT_HOME` — the config dir holding both the user's `settings.json` (where
    /// the slot lives) and the double's own registry state.
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

    /// Run the fixture with `stdin` piped in — the status-line entrypoint's real
    /// calling convention.
    ///
    /// This one call gets the inherited `PATH` appended, because composing runs the
    /// user's own status command through the platform shell and the curated `PATH`
    /// above carries no `sh`/`cmd`. Safe to widen here and nowhere else: the
    /// status-line entrypoint never detects or writes a backend, so a stray harness CLI
    /// on the dev box cannot reach it.
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
        self.copilot_home.join("settings.json")
    }

    fn seed_settings(&self) {
        fs::write(self.settings_path(), SEED_SETTINGS).unwrap();
    }

    fn settings(&self) -> String {
        fs::read_to_string(self.settings_path()).unwrap()
    }

    fn status_line(&self) -> Value {
        serde_json::from_str::<Value>(&self.settings()).unwrap().get("statusLine").cloned().unwrap_or(Value::Null)
    }

    /// Overwrite the slot with someone else's value, as a second tool (or the user)
    /// would.
    fn set_status_line(&self, value: Value) {
        let mut parsed: Value = serde_json::from_str(&self.settings()).unwrap();
        parsed["statusLine"] = value;
        fs::write(self.settings_path(), serde_json::to_vec_pretty(&parsed).unwrap()).unwrap();
    }

    /// The copilot-cli agent's stamp marker, if one exists. Its presence is what says
    /// this backend considers the plugin installed.
    fn marker(&self) -> Option<Value> {
        let markers = self.data.join("ez-fixture-plugin").join("markers");
        let entries: Vec<_> = fs::read_dir(&markers).map(|d| d.flatten().collect()).unwrap_or_default();
        entries.into_iter().find_map(|entry| {
            let marker: Value = serde_json::from_slice(&fs::read(entry.path()).unwrap_or_default()).ok()?;
            (marker.get("agent").and_then(Value::as_str) == Some("copilot-cli")).then_some(marker)
        })
    }

    fn stashed_original(&self) -> Value {
        self.marker().and_then(|m| m.get("statusline_original").cloned()).unwrap_or(Value::Null)
    }

    /// Take the `copilot` double off the scratch PATH — the user uninstalling the
    /// Copilot CLI itself, which makes every copilot-cli row a `NotDetected` skip.
    fn remove_copilot_double(&self) {
        let binary = self.root.join("bin").join(format!("copilot{}", std::env::consts::EXE_SUFFIX));
        fs::remove_file(&binary).unwrap();
    }

    /// Drop every plugin from the `fake_copilot` registry, leaving the marketplace —
    /// what a hand-run `copilot plugin uninstall` leaves behind.
    fn drop_plugin_from_registry(&self) {
        let path = self.copilot_home.join("fake-copilot-state.json");
        let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        state["plugins"] = json!([]);
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

/// `curated` first (so the scratch `copilot` still wins), then whatever the test
/// process inherited.
fn with_inherited_path(curated: &OsString) -> OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(curated).chain(std::env::split_paths(&inherited)).collect();
    std::env::join_paths(&dirs).unwrap_or_else(|_| curated.clone())
}

fn seed_status_line() -> Value {
    serde_json::from_str::<Value>(SEED_SETTINGS).unwrap().get("statusLine").cloned().unwrap()
}

/// The value the backend must have written: copilot's single-object command shape
/// (byte-identical to CC's) with `${AGENTGEAR_CLIENT}` already expanded.
fn our_status_line() -> Value {
    json!({"type": "command", "command": OUR_COMMAND, "padding": 0})
}

/// What host version N left in the slot: our command, a padding this version no longer
/// renders. Ownership is the command string alone, so this is still OURS.
fn our_earlier_rendering() -> Value {
    json!({"type": "command", "command": OUR_COMMAND, "padding": 1})
}

#[test]
fn copilot_cli_statusline_full_lifecycle() {
    let env = Env::new("lifecycle");
    env.seed_settings();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // Our declaration owns the slot, `${AGENTGEAR_CLIENT}` expanded to this backend.
    assert_eq!(env.status_line(), our_status_line(), "our statusLine did not land:\n{}", env.settings());
    assert_eq!(env.stashed_original(), seed_status_line(), "install did not stash the user's original");
    // The user's unrelated key survived the read-modify-write.
    assert!(env.settings().contains("theirSetting"), "seeded top-level key was clobbered:\n{}", env.settings());

    // The compose helper runs the displaced command and appends its rows under ours.
    let (ok, out) = env.fixture_stdin(&["statusline", "--client", "copilot-cli"], SESSION_JSON);
    assert!(ok, "statusline subcommand failed: {out}");
    assert_eq!(out, "ez-fixture row\ntheir-bar-row", "compose did not stack our row over the user's");

    // doctor names the slot's owner, from the same shared check every slot backend ends on.
    let (_, report) = env.fixture(&["doctor"]);
    assert!(report.contains("[ ok ] status line installed"), "doctor did not report the slot as installed:\n{report}");
    assert!(report.contains("owns the statusLine slot"), "doctor did not recognise our value as ours:\n{report}");

    // Idempotent: a second identical reconcile writes nothing.
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    // The user's own status line is back, byte for byte, and so is the whole file.
    assert_eq!(env.status_line(), seed_status_line(), "uninstall did not restore the user's statusLine");
    assert_eq!(env.settings(), SEED_SETTINGS, "uninstall did not restore settings.json byte-for-byte");
}

#[test]
fn copilot_cli_statusline_stash_survives_update_and_self_heal() {
    // The mutation this pins: `stamp::write` rebuilds the marker from scratch on every
    // install/update/self-heal, so without an explicit carry-forward the displaced
    // original is erased by the first `update` and uninstall silently deletes the user's
    // status line instead of restoring it.
    let env = Env::new("stash-survives");
    env.seed_settings();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let (ok, out) = env.fixture(&["update"]);
    assert!(ok, "update failed: {out}");
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");

    // Still ours between the two, so the restore below is a real restore.
    assert_eq!(env.status_line(), our_status_line(), "update/self-heal disturbed our statusLine");
    assert_eq!(env.stashed_original(), seed_status_line(), "the stash did not survive update + self-heal");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(env.settings(), SEED_SETTINGS, "uninstall did not restore settings.json byte-for-byte");
}

#[test]
fn copilot_cli_statusline_padding_drift_is_repaired_without_losing_the_stash() {
    // A padding this version no longer renders is drift behind a healthy registry:
    // probe must read it as `NeedsRepair`, reconcile must converge it, and the earlier
    // stash must NOT be overwritten by our own drifted value.
    let env = Env::new("drift");
    env.seed_settings();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    env.set_status_line(our_earlier_rendering());
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored on a drifted statusLine: {out}");
    assert_eq!(out, "Repaired", "drifted padding behind a healthy registry is a repair, got {out}");
    assert_eq!(env.status_line(), our_status_line(), "self-heal did not converge our own rendering");
    assert_eq!(env.stashed_original(), seed_status_line(), "the repair pass overwrote the user's stashed original");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(env.settings(), SEED_SETTINGS, "the repair pass lost the user's original");
}

#[test]
fn copilot_cli_statusline_our_own_earlier_rendering_is_never_stashed() {
    // The failure this pins: ownership decided by whole-value equality reads our OWN
    // previous rendering as "the user's original" the moment a host release changes its
    // padding or its flags. That destroys the user's value AND poisons the stash with
    // our own command, which `compose` would then run from inside itself, on every turn
    // the harness re-renders. A suite that installs and uninstalls at ONE version
    // proves nothing here — only the version change reaches it.
    let env = Env::new("own-rendering");
    env.seed_settings();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert_eq!(env.stashed_original(), seed_status_line(), "install did not stash the user's original");

    env.set_status_line(our_earlier_rendering());

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup errored on our own drifted rendering: {out}");
    assert_eq!(out, "Repaired", "our own drifted rendering is drift, got {out}");
    assert_eq!(env.status_line(), our_status_line(), "setup did not converge our own rendering");

    // Asserted BEFORE the stash below on purpose: this is the anti-recursion guard's
    // positive control. Break the ownership test and the stash holds OUR command, so
    // this call is what would re-enter the binary instead of returning a row.
    let (ok, out) = env.fixture_stdin(&["statusline", "--client", "copilot-cli"], SESSION_JSON);
    assert!(ok, "statusline subcommand failed: {out}");
    assert_eq!(out, "ez-fixture row\ntheir-bar-row", "compose lost the user's row");

    assert_eq!(env.stashed_original(), seed_status_line(), "our own earlier rendering was stashed as the user's original");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(env.settings(), SEED_SETTINGS, "uninstall left a command pointing at the uninstalled binary");
}

#[test]
fn copilot_cli_statusline_manual_plugin_uninstall_restores_the_user_line() {
    // self_heal's "plugin already gone under our marker" row clears the marker, and the
    // stash goes with it. Without `forget` restoring first, the user is left with our
    // command pointing at an uninstalled plugin and no copy of their own value anywhere
    // on disk. This is the row that reds if `forget` regresses to the trait default:
    // `remove` is never called here, so nothing else can put their line back.
    let env = Env::new("manual-uninstall");
    env.seed_settings();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
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
    assert!(env.marker().is_none(), "self-heal re-adopted a plugin that is not installed");
}

#[test]
fn copilot_cli_statusline_uninstall_restores_when_the_harness_itself_is_gone() {
    // `uninstall`'s skip rows never call `remove`, so a user who uninstalled the Copilot
    // CLI before running our uninstall would keep our command in settings.json while the
    // marker holding their original is cleared out from under it. The slot lives in the
    // user's own settings file, which resolves with no `copilot` on PATH at all.
    let env = Env::new("harness-gone");
    env.seed_settings();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert_eq!(env.stashed_original(), seed_status_line(), "install did not stash the user's original");

    env.remove_copilot_double();

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall errored with the harness gone: {out}");
    assert_eq!(out, "NoOp", "every agent should skip with nothing detected, got {out}");
    assert_eq!(env.status_line(), seed_status_line(), "a skipped uninstall stranded our command and dropped the stash");
    assert_eq!(env.settings(), SEED_SETTINGS, "settings.json was not restored byte-for-byte");
    assert!(env.marker().is_none(), "a skipped uninstall orphaned its marker");
}

#[test]
fn copilot_cli_statusline_remove_leaves_a_foreign_value_alone() {
    // Exact-remove: once someone else owns the slot, uninstall must not touch it — not
    // even to restore what we stashed, which is no longer what the user sees.
    let env = Env::new("foreign");
    env.seed_settings();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let foreign = json!({"type": "command", "command": "someone-elses-bar"});
    env.set_status_line(foreign.clone());

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(env.status_line(), foreign, "uninstall clobbered a statusLine that was no longer ours");
}

#[test]
fn copilot_cli_statusline_self_heal_after_uninstall_re_adds_nothing() {
    // Never resurrect. The user's own line is back in the slot after uninstall, and a
    // foreign value is no evidence our plugin is installed — count it as presence and
    // self_heal's adopt row reinstalls the whole translation over a plugin the user
    // deliberately removed. copilot's registry settles presence on its own, which is the
    // belt to that braces.
    let env = Env::new("no-resurrect");
    env.seed_settings();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored after uninstall: {out}");
    assert_eq!(out, "NoOp", "self-heal resurrected an uninstalled plugin, got {out}");
    assert_eq!(env.settings(), SEED_SETTINGS, "self-heal retook the slot after uninstall");
    assert!(env.marker().is_none(), "self-heal adopted a plugin that is not installed");
}

#[test]
fn copilot_cli_statusline_is_user_scope_only() {
    // copilot's repo-scope settings files exist, but whether `statusLine` is
    // repo-overridable is built into its Rust `runtime.node` and could not be
    // enumerated — so a project slot write would be inert config dropped in someone's
    // repo. The backend declares user scope only, which makes a project pass a visible
    // skip.
    //
    // A user-scope install FIRST is what makes the scope guard observable at all. The
    // project teardown still reaches `forget` (skips included), and the stash is keyed
    // on `(plugin, scope, client)`: drop the guard and that call reads an EMPTY
    // project-scope marker, still sees our command as `is_ours`, and deletes the slot —
    // so the user-scope uninstall below has nothing left to restore from. Without the
    // user-scope install, the seeded slot is foreign to us and the whole mutation is
    // invisible.
    let env = Env::new("project-scope");
    env.seed_settings();
    let project = env.root.join("project");
    fs::create_dir_all(&project).unwrap();
    let project_str = project.display().to_string();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "user-scope setup failed: {out}");

    let (ok, report) = env.fixture(&["setup-report", "--agent", "copilot-cli", "--project", &project_str]);
    assert!(ok, "project-scope setup failed: {report}");
    assert_eq!(report.trim(), "copilot-cli: skipped (no config surface at this scope)", "the scope skip is not visible:\n{report}");
    assert_eq!(env.status_line(), our_status_line(), "a project-scope install rewrote the USER's slot");

    let (ok, out) = env.fixture(&["uninstall", "--project", &project_str]);
    assert!(ok, "project-scope uninstall failed: {out}");
    assert_eq!(env.status_line(), our_status_line(), "a project-scope teardown undid the user-scope slot");

    // The user-scope install is still whole, so its own teardown can still restore.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "user-scope uninstall failed: {out}");
    assert_eq!(env.settings(), SEED_SETTINGS, "the project-scope pass consumed the user-scope stash");
}

#[test]
fn copilot_cli_empty_config_dir_is_rejected_without_stranding_other_backends() {
    // Positive control first: the identical install with a real `COPILOT_HOME` must
    // succeed, so the rejection below is provably about the empty value and not about
    // this harness never installing at all.
    let env = Env::new("empty-config-dir");
    env.seed_settings();
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "control install with a real COPILOT_HOME should succeed, got {out}");
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "control uninstall should succeed, got {out}");

    // gemini is HOME-based detection (`~/.gemini`), so it needs no CLI double on
    // PATH — a second, working backend to prove the empty override only fails
    // copilot-cli.
    fs::create_dir_all(env.root.join(".gemini")).unwrap();

    let mut cmd = Command::new(BIN);
    cmd.args(["setup-report", "--agent", "copilot-cli", "--agent", "gemini"]);
    env.apply(&mut cmd);
    cmd.env("COPILOT_HOME", "");
    // An empty override resolves against the current directory — the exact behavior
    // under test — so cwd is pinned to the scratch root; without this a rejected
    // resolver would still leave `fake_copilot`'s CWD-relative registry write behind in
    // the real crate directory instead of a directory that gets torn down.
    cmd.current_dir(&env.root);
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();

    assert!(!out.status.success(), "an empty COPILOT_HOME must fail the fan-out:\n{stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.iter().any(|l| l.starts_with("copilot-cli: failed: ") && l.contains("COPILOT_HOME")),
        "copilot-cli's failure must name the empty variable:\n{stdout}"
    );
    assert!(lines.contains(&"gemini: installed"), "gemini must still install despite copilot-cli failing:\n{stdout}");

    // The bug this whole feature exists to fix: the reject must fire BEFORE any
    // `copilot` CLI call, not after a marketplace-add/install already ran. If it fires
    // late, `fake_copilot`'s own state file (its registry) exists at the pinned cwd
    // despite the reconcile reporting `failed`.
    assert!(
        !env.root.join(FAKE_COPILOT_STATE_FILENAME).exists(),
        "copilot-cli's registry must be untouched when the empty override is rejected"
    );
}

#[test]
fn copilot_cli_statusline_doctor_reads_empty_config_dir_as_a_fail_not_a_warn() {
    let env = Env::new("doctor-empty-config-dir");
    env.seed_settings();

    let mut cmd = Command::new(BIN);
    cmd.args(["doctor"]);
    env.apply(&mut cmd);
    cmd.env("COPILOT_HOME", "");
    cmd.current_dir(&env.root);
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();

    assert!(!out.status.success(), "doctor must exit unhealthy when the override is rejected:\n{stdout}");
    assert!(
        stdout.lines().any(|l| l.starts_with("[fail] status line installed: ") && l.contains("COPILOT_HOME")),
        "doctor must report the empty override as a Fail naming the variable, not a Warn:\n{stdout}"
    );
    assert!(!stdout.contains("[warn] status line installed"), "the empty override must not read as a Warn:\n{stdout}");
    assert!(!env.root.join(FAKE_COPILOT_STATE_FILENAME).exists(), "doctor must never write anything for a read-only check");
}

#[test]
fn copilot_cli_teardown_with_nothing_installed_writes_no_file() {
    // A teardown that owns nothing must not create a settings file in the user's config
    // dir at all — `forget` runs unconditionally, so an eager write here would leave a
    // file behind for a plugin that was never installed.
    let env = Env::new("empty-teardown");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall errored with nothing installed: {out}");
    assert_eq!(out, "NoOp", "a teardown with nothing of ours to undo must report no change, got {out}");
    assert!(!env.settings_path().exists(), "teardown created a settings file it had nothing to undo in");
}
