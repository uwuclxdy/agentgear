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

/// Mirrors `fake_claude.rs`'s own `STATE_FILENAME`; a bin target exports nothing an
/// external test crate can import, so this is a second literal by necessity, not by
/// oversight (same idiom as `fake_claude_config_dir.rs`'s own copy).
const FAKE_CLAUDE_STATE_FILENAME: &str = "fake-claude-state.json";

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

/// The same file with the slot never written: a user who has settings of their own
/// but no status line at all. Install displaces nothing here, so uninstall has no
/// original to put back and must take the key out instead.
const SEED_SETTINGS_NO_STATUS_LINE: &str = r#"{
  "theirSetting": true
}
"#;

/// A session payload shaped like Claude Code's: `compose` reads `cwd` off it to pick
/// project-then-user scope, and pipes the whole thing to the user's own command.
const SESSION_JSON: &str = r#"{"session_id":"abc","cwd":"/nonexistent/project"}"#;

/// What the fixture host declares, with `${AGENTGEAR_CLIENT}` expanded for claude.
const OUR_COMMAND: &str = "host_fixture statusline --client claude";

/// The env var the fixture host reads its status-line subcommand name out of, and the
/// name a "later release" of it uses. Deliberately not a prefix or suffix of
/// `statusline`: the ownership record is compared for equality, and a name that
/// contains the old one would pass a substring bug too.
const RENAME_VAR: &str = "EZ_FIXTURE_STATUSLINE_SUBCOMMAND";
const RENAMED_SUBCOMMAND: &str = "bar";
const RENAMED_COMMAND: &str = "host_fixture bar --client claude";

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
        Self::seeded(name, SEED_SETTINGS)
    }

    /// Start from settings holding no status line, so install displaces nothing and
    /// uninstall reaches the delete half of `remove`. Every other test here seeds one,
    /// which leaves install something to displace and uninstall something to restore.
    fn without_status_line(name: &str) -> Self {
        Self::seeded(name, SEED_SETTINGS_NO_STATUS_LINE)
    }

    fn seeded(name: &str, settings: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-cc-statusline-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let env = Env { cfg: root.join("cfg"), data: root.join("data"), run: root.join("run"), path: curated_path(&bin), root };
        for dir in [&bin, &env.cfg, &env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::copy(FAKE_CLAUDE, bin.join(format!("claude{}", std::env::consts::EXE_SUFFIX))).unwrap();
        fs::write(env.settings_path(), settings).unwrap();
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

    /// The same run with the fixture host declaring a RENAMED status-line subcommand,
    /// standing in for a later release of the host that changed its own command string.
    /// Both halves move together, exactly as they would in a real release: what the
    /// declaration renders into the slot, and what the binary answers to.
    ///
    /// Returns stderr alongside stdout, unlike [`Self::fixture`]: the fixture prints a
    /// failed lifecycle's error there, so a caller asserting a run FAILED can only check
    /// that it failed for the reason it meant to test by reading it.
    fn fixture_renamed(&self, args: &[&str], subcommand: &str) -> (bool, String, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        cmd.env(RENAME_VAR, subcommand);
        let out = cmd.output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
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
        self.fixture_stdin_env(args, stdin, &[])
    }

    /// The same run with `extra` forced into the host's environment, for the cases where
    /// what the status-line entrypoint INHERITS is the thing under test.
    fn fixture_stdin_env(&self, args: &[&str], stdin: &str, extra: &[(&str, &str)]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        self.apply(&mut cmd);
        cmd.env("PATH", with_inherited_path(&self.path));
        for (key, value) in extra {
            cmd.env(key, value);
        }
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

    /// The claude backend's whole stamp marker, or `None` when it stamped none. Kept
    /// apart from [`Self::stashed_original`] because the two answer different
    /// questions: a marker holding no stash and no marker at all both read as "nothing
    /// stashed", and only one of them is a state the lifecycle produces.
    fn claude_marker(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&fs::read(self.claude_marker_path()?).ok()?).ok()
    }

    /// The file [`Self::claude_marker`] reads, so a test can edit it in place.
    fn claude_marker_path(&self) -> Option<PathBuf> {
        let markers = self.data.join("ez-fixture-plugin").join("markers");
        let entries: Vec<_> = fs::read_dir(&markers).map(|d| d.flatten().collect()).unwrap_or_default();
        entries
            .into_iter()
            .find(|entry| {
                let marker: serde_json::Value =
                    serde_json::from_slice(&fs::read(entry.path()).unwrap_or_default()).unwrap_or(serde_json::Value::Null);
                marker.get("agent").and_then(serde_json::Value::as_str) == Some("claude")
            })
            .map(|entry| entry.path())
    }

    /// Take the ownership record back out of the marker, leaving every other field —
    /// byte-for-byte the shape any install stamped before `statusline_command` existed
    /// left on disk.
    fn strip_recorded_command(&self) {
        let path = self.claude_marker_path().expect("install must stamp a claude marker");
        let mut marker: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        marker.as_object_mut().unwrap().remove("statusline_command");
        fs::write(&path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();
    }

    /// Overwrite the marker's stash so it names `command`, the shape a release that
    /// renamed its own status-line subcommand leaves behind.
    ///
    /// By hand because the fixture's rename knob cannot produce a RE-ENTERING stash: it
    /// changes the subcommand NAME, and the spawned level inherits
    /// `EZ_FIXTURE_STATUSLINE_SUBCOMMAND`, so a stash naming the old name misses
    /// `statusline_subcommand()` and lands on the usage arm instead of composing again.
    /// Only an ADDITIVE rename re-enters, and the fixture declares no additive variant.
    ///
    /// Our own command reaching the stash at all is a different thing and needs no hand
    /// edit — `strip_recorded_command` then a renamed `setup` gets there through the real
    /// lifecycle. What prevents that in production is `statuslinejson::is_ours` against
    /// the marker's command record, NOT `statusline::is_own_command`, which only refuses
    /// to run a stash once it is already poisoned.
    fn set_stashed_original(&self, command: &str) {
        let path = self.claude_marker_path().expect("install must stamp a claude marker");
        let mut marker: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        marker["statusline_original"] = serde_json::json!({"type": "command", "command": command, "padding": 0});
        fs::write(&path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();
    }

    /// The stash the claude backend recorded, as raw JSON.
    fn stashed_original(&self) -> serde_json::Value {
        self.claude_marker().and_then(|m| m.get("statusline_original").cloned()).unwrap_or(serde_json::Value::Null)
    }

    /// The command the claude backend's marker says it last wrote into the slot.
    fn recorded_command(&self) -> serde_json::Value {
        self.claude_marker().and_then(|m| m.get("statusline_command").cloned()).unwrap_or(serde_json::Value::Null)
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

/// What the renamed release writes into the slot.
fn renamed_status_line() -> serde_json::Value {
    serde_json::json!({"type": "command", "command": RENAMED_COMMAND, "padding": 0})
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

/// A stash that bumps `counter` and then re-enters the host, which is what the two
/// depth tests below drive.
///
/// The re-entry is an ADDITIVE rename, built with no fixture change: `flag_value` ignores
/// flags it does not know, so `--legacy` differs from the declaration as a STRING while
/// still reaching the same status-line arm. The counter bump is what makes the spawns
/// countable — the rows alone cannot separate "bounded at one re-entry" from "level 0
/// gave up on a timeout".
///
/// The `-le 3` bound is a safety belt on the FAILING path, not part of the contract:
/// `reap` kills only the direct child and sets no process group, so a regression orphans
/// each level's `host_fixture` to init, and an orphan spawns its successor during
/// `compose` before dying of EPIPE on its own final write. The frontier is self-sustaining
/// and outlives the test binary. Bounded, a regression reds at depth ~4 with nothing left
/// running; unbounded it needs an RLIMIT_NPROC cap to be safe to run at all, and that cap
/// is per-uid — it fails every parallel process on the box. The counter still
/// discriminates (4 vs 1). Paths are quoted because `temp_dir` honours `TMPDIR`, which may
/// contain a space.
#[cfg(not(windows))]
fn reentering_stash(counter: &Path) -> String {
    format!("echo x >> \"{p}\"; [ $(wc -l < \"{p}\") -le 3 ] && host_fixture statusline --client claude --legacy", p = counter.display())
}

/// Gated for the same reason `tests/unit/statusline.rs`'s runner mod is: the stash under
/// test has to bump a counter AND re-enter the host in one command string, and `a; b`
/// chaining is POSIX-only. The guard itself is platform-blind — an env var on the child.
#[cfg(not(windows))]
#[test]
fn claude_statusline_an_earlier_releases_stash_re_enters_at_most_once() {
    // `is_own_command` compares a stash against the command the host declares NOW, so a
    // stash naming what an EARLIER release declared reads as foreign and gets run. When
    // that rename was additive — one more flag on the same subcommand — the old string
    // still dispatches to this binary's own status-line arm, which reads the same stash
    // and spawns again. Each level owns a fresh 3s timeout and `reap` kills only its
    // direct child, so nothing upstream cancels the chain — in production it runs away.
    // The stash below bounds it so this test cannot (see the comment there).
    //
    // Reachable with no wrong code anywhere: an install stamped before
    // `statusline_command` existed carries no ownership record, so the first renamed
    // release reads its own slot value as foreign and stashes precisely this.
    let env = Env::new("earlier-release-stash");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let spawns = env.root.join("spawns");
    env.set_stashed_original(&reentering_stash(&spawns));

    let (ok, out) = env.fixture_stdin(&["statusline", "--client", "claude"], SESSION_JSON);
    assert!(ok, "statusline subcommand failed: {out}");

    // Doubles as the positive control: a stash that silently failed to run at all would
    // otherwise satisfy every count below for the wrong reason.
    let spawned = fs::read_to_string(&spawns).expect("the stash must actually have been spawned");
    assert_eq!(spawned.lines().count(), 1, "the stash re-entered the host more than once:\n{spawned}");
    // Depth 1 is the whole contract: level 1 renders its own row and spawns nothing, so
    // the bar carries our row twice. A bounded duplicate is the accepted outcome.
    assert_eq!(out, "ez-fixture row\nez-fixture row", "the bounded re-entry must render our own row twice and nothing else");
}

/// POSIX-only for the same reason as its sibling: the stash chains two commands.
#[cfg(not(windows))]
#[test]
fn claude_statusline_a_blank_nested_sentinel_does_not_suppress_the_users_row() {
    // The sentinel is presence-only and we only ever write "1", so a BLANK value cannot
    // be ours — it means something else in the environment exported the name. Read as
    // present, it refuses to spawn the user's command on every render of every host,
    // dropping their row with nothing observable from outside to explain it. Read as
    // absent, the only value that can misfire is one we never write.
    //
    // Both directions ride in one run: level 0 inherits the blank value and must still
    // spawn, level 1 gets the real "1" that `run_with_timeout` sets and must still refuse.
    // So this pins the empty case without weakening the depth bound underneath it.
    let env = Env::new("blank-sentinel");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let spawns = env.root.join("spawns");
    env.set_stashed_original(&reentering_stash(&spawns));

    let (ok, out) = env.fixture_stdin_env(&["statusline", "--client", "claude"], SESSION_JSON, &[("AGENTGEAR_STATUSLINE_NESTED", "")]);
    assert!(ok, "statusline subcommand failed: {out}");

    // Under a bare `is_some()` read the blank value suppresses level 0's spawn outright,
    // so the counter file is never created and this is the assertion that reds.
    let spawned = fs::read_to_string(&spawns).expect("a blank sentinel must not suppress the user's own command");
    assert_eq!(spawned.lines().count(), 1, "the real sentinel stopped bounding the chain:\n{spawned}");
    assert_eq!(out, "ez-fixture row\nez-fixture row", "the user's row went missing behind a blank sentinel");
}

#[test]
fn claude_statusline_a_converged_slot_still_records_its_command() {
    // The record is written on EVERY successful settings write, not only on one that
    // changed the file — and the population that depends on the difference is exactly
    // the one this whole feature exists for. An install stamped before
    // `statusline_command` existed already holds the current command in its slot, so its
    // next `setup` writes nothing and reports `NoOp`. Gate the record on that `changed`
    // flag and such an install never acquires one at all, and the next release's rename
    // then reads its own value as foreign and destroys the user's original.
    //
    // Nothing else in the workspace reds on that one-word gate: every other status-line
    // test either changes the slot on the pass it asserts, or was installed by a binary
    // that already recorded. Only the no-op pass separates the two.
    let env = Env::new("record-on-noop");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // Both halves of the pre-record state, asserted rather than assumed: no record on
    // the marker, and a slot already holding what this version would write. Either one
    // alone reaches a different branch.
    env.strip_recorded_command();
    assert_eq!(env.recorded_command(), serde_json::Value::Null, "the arm under test needs the record gone");
    assert_eq!(env.status_line(), our_status_line(), "and the slot already holding what this version writes");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok, "setup over a converged slot failed: {out}");
    assert_eq!(out, "NoOp", "the arm under test needs a pass that writes nothing, got {out}");
    assert_eq!(
        env.recorded_command(),
        serde_json::json!(OUR_COMMAND),
        "a reconcile that changed nothing left the slot with no owner across the next rename"
    );
}

#[test]
fn claude_statusline_a_refused_settings_write_records_no_command() {
    // The ownership record is only true of the slot if the write it describes landed,
    // and one designed, documented, recoverable path breaks that: `json_edit` refuses an
    // unparseable settings file with `Error::Config`, while `read_settings` reads that
    // same file as an EMPTY slot. Record before the write and a run that fails there
    // leaves a marker naming a command nothing ever wrote — overwriting the still-true
    // record of what really is in the slot. The next rename then reads the real value as
    // foreign and stashes it over the user's original, which is the exact loss the
    // record exists to prevent.
    //
    // The claude backend is where this is reachable: its `statusLine` slot is its only
    // write outside the plugin registry, so a corrupt settings file fails on the slot
    // and nowhere earlier.
    let env = Env::new("refused-write");

    // Version N takes the slot and stashes the user's own line.
    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert_eq!(env.stashed_original(), seed_status_line(), "install did not stash the user's original");
    assert_eq!(env.recorded_command(), serde_json::json!(OUR_COMMAND), "install did not record the command it wrote");
    let installed = env.settings();

    // Their settings are momentarily unparseable — a half-finished hand edit, a
    // truncated write — and version N+1 runs `setup` while they are.
    fs::write(env.settings_path(), "{\n  \"theirSetting\": true,\n").unwrap();
    let (ok, out, err) = env.fixture_renamed(&["setup", "--agent", "claude"], RENAMED_SUBCOMMAND);
    assert!(!ok, "an unparseable settings file must refuse the install, got {out}");
    // Asserted on the REASON, not just on the failure: this test is only about the
    // window between the marker write and the settings write, so a future change that
    // failed the reconcile earlier (before the slot code ran at all) would otherwise
    // leave it green while proving nothing.
    assert!(
        err.contains("could not parse config") && err.contains(&env.settings_path().display().to_string()),
        "the install must have failed on the settings-file parse, not somewhere earlier:\n{err}"
    );
    assert_eq!(
        env.recorded_command(),
        serde_json::json!(OUR_COMMAND),
        "a refused settings write recorded a command it never wrote, discarding the true record"
    );

    // They fix their JSON. The slot still holds version N's command and the stash still
    // holds theirs, so the run below is version N+1 meeting version N's real value.
    fs::write(env.settings_path(), &installed).unwrap();
    assert_eq!(env.status_line(), our_status_line(), "the refused write must have left the slot alone");

    let (ok, out, err) = env.fixture_renamed(&["setup", "--agent", "claude"], RENAMED_SUBCOMMAND);
    assert!(ok, "setup after the settings file was fixed failed: {out}{err}");
    assert_eq!(env.status_line(), renamed_status_line(), "the renamed release did not converge the slot to its own command");
    assert_eq!(
        env.stashed_original(),
        seed_status_line(),
        "the refused write's stale record stashed our own command over the user's original"
    );

    // And the whole point: what comes back is the user's own line, byte for byte.
    let (ok, out, err) = env.fixture_renamed(&["uninstall"], RENAMED_SUBCOMMAND);
    assert!(ok && out == "Removed", "uninstall failed: {out}{err}");
    assert_eq!(env.settings(), SEED_SETTINGS, "the user's settings did not come back");
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

#[test]
fn claude_statusline_empty_config_dir_is_rejected_without_stranding_other_backends() {
    // Positive control first: the identical install with a real `CLAUDE_CONFIG_DIR`
    // must succeed, so the rejection below is provably about the empty value and not
    // about this harness never installing at all.
    let env = Env::new("empty-config-dir");
    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "control install with a real CLAUDE_CONFIG_DIR should succeed, got {out}");
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "control uninstall should succeed, got {out}");

    // gemini is HOME-based detection (`~/.gemini`), so it needs no CLI double on
    // PATH — a second, working backend to prove the empty override only fails claude.
    fs::create_dir_all(env.root.join(".gemini")).unwrap();

    let mut cmd = Command::new(BIN);
    cmd.args(["setup-report", "--agent", "claude", "--agent", "gemini"]);
    env.apply(&mut cmd);
    cmd.env("CLAUDE_CONFIG_DIR", "");
    // An empty override resolves against the current directory — the exact behavior
    // under test — so cwd is pinned to the scratch root; without this a rejected
    // resolver would still leave `fake_claude`'s CWD-relative registry write behind in
    // the real crate directory instead of a directory that gets torn down.
    cmd.current_dir(&env.root);
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();

    assert!(!out.status.success(), "an empty CLAUDE_CONFIG_DIR must fail the fan-out:\n{stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.iter().any(|l| l.starts_with("claude: failed: ") && l.contains("CLAUDE_CONFIG_DIR")),
        "claude's failure must name the empty variable:\n{stdout}"
    );
    assert!(lines.contains(&"gemini: installed"), "gemini must still install despite claude failing:\n{stdout}");

    // The bug this whole feature exists to fix: the reject must fire BEFORE any
    // `claude` CLI call, not after a marketplace-add/install already ran. If it fires
    // late, `fake_claude`'s own state file (its registry) exists at the pinned cwd
    // despite the reconcile reporting `failed`.
    assert!(!env.root.join(FAKE_CLAUDE_STATE_FILENAME).exists(), "claude's registry must be untouched when the empty override is rejected");
}

#[test]
fn claude_statusline_doctor_reads_empty_config_dir_as_a_fail_not_a_warn() {
    let env = Env::new("doctor-empty-config-dir");

    let mut cmd = Command::new(BIN);
    cmd.args(["doctor"]);
    env.apply(&mut cmd);
    cmd.env("CLAUDE_CONFIG_DIR", "");
    cmd.current_dir(&env.root);
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();

    assert!(!out.status.success(), "doctor must exit unhealthy when the override is rejected:\n{stdout}");
    assert!(
        stdout.lines().any(|l| l.starts_with("[fail] status line installed: ") && l.contains("CLAUDE_CONFIG_DIR")),
        "doctor must report the empty override as a Fail naming the variable, not a Warn:\n{stdout}"
    );
    assert!(!stdout.contains("[warn] status line installed"), "the empty override must not read as a Warn:\n{stdout}");
    assert!(!env.root.join(FAKE_CLAUDE_STATE_FILENAME).exists(), "doctor must never write anything for a read-only check");
}

/// A declaration-free host (no `statusline_fn` at all) must keep installing normally
/// under an empty `CLAUDE_CONFIG_DIR`: the reject is gated behind the host actually
/// declaring a status line (`statuslinejson::target`'s own condition), so a host with
/// no slot to protect has nothing misplaced by the empty override — `claude` resolves
/// its own config dir independently of ours. `embedded_github_fixture` is the fixture
/// binary with no `statusline_fn`.
#[test]
fn declaration_free_host_still_installs_under_empty_config_dir() {
    const NO_SLOT_BIN: &str = env!("CARGO_BIN_EXE_embedded_github_fixture");

    let root = std::env::temp_dir().join(format!("ez-cc-no-slot-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let bin = root.join("bin");
    for dir in [&bin, &root.join("config"), &root.join("data"), &root.join("run")] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::copy(FAKE_CLAUDE, bin.join(format!("claude{}", std::env::consts::EXE_SUFFIX))).unwrap();

    let mut cmd = Command::new(NO_SLOT_BIN);
    cmd.args(["install"])
        .env("CLAUDE_CONFIG_DIR", "")
        .env("HOME", &root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("PATH", curated_path(&bin))
        // Same discipline as the rejected-override test above: pin cwd so a passing
        // run cannot leave `fake_claude`'s cwd-relative registry write behind either.
        .current_dir(&root);
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();

    assert!(out.status.success(), "a declaration-free host must install fine under an empty CLAUDE_CONFIG_DIR:\n{stdout}");
    assert!(stdout.lines().any(|l| l == "claude: installed"), "claude must actually install, not just skip:\n{stdout}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn claude_statusline_uninstall_deletes_the_slot_with_nothing_stashed() {
    // The other arm of the branch every test above takes: install onto an empty slot
    // displaces nothing, so uninstall has no original to put back and has to delete
    // the key. Leaving it behind strands a command pointing at the uninstalled binary,
    // and parking it at `null` is no better — the key is still there for the harness
    // to read, and it is a write to the user's file on a teardown.
    let env = Env::without_status_line("no-stash");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert_eq!(env.status_line(), our_status_line(), "our statusLine did not land:\n{}", env.settings());
    // The marker must EXIST with the field absent — the input shape uninstall actually
    // reads here. "No marker at all" reaches the same branch, through a state the
    // lifecycle never produces, so asserting only the stash would let this test decay
    // into the weaker one without ever going red.
    let marker = env.claude_marker().expect("install must stamp a claude marker");
    assert!(marker.get("statusline_original").is_none(), "an empty slot must stash nothing, got marker: {marker}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    // Asserted on the object, not through `status_line()`: that helper folds a missing
    // key and an explicit `null` onto the same `Value::Null`, so it cannot tell a
    // removal from a key parked at null.
    let parsed: serde_json::Value = serde_json::from_str(&env.settings()).unwrap();
    assert!(
        !parsed.as_object().unwrap().contains_key("statusLine"),
        "uninstall left the slot key behind instead of deleting it:\n{}",
        env.settings()
    );
    assert_eq!(env.settings(), SEED_SETTINGS_NO_STATUS_LINE, "uninstall did not restore settings.json byte-for-byte");
}
