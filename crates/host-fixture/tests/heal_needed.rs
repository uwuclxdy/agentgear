//! Hermetic proof that `heal_needed_report()` is spawn-free, answers the
//! `claude` verdict it should, ends a lingering reinstall on the healthy row, and
//! mirrors self_heal's skip gates.
//!
//! The host's hot-path gate (a SessionStart hook) wants to run the heal
//! only when it would act, because a heal pays two `claude` spawns on every
//! session event. The pre-check must answer from files alone, so the spawn test
//! runs a real `setup` through the `fake_claude` double, then calls the
//! `heal-needed` subcommand and asserts it added no line to the double's call
//! log. The double models its own state file, not CC's registry files, so this
//! pins spawn-freedom, never the verdict itself (the verdict is pinned by the
//! classifier's unit tests).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");
const FAKE_CLAUDE: &str = env!("CARGO_BIN_EXE_fake_claude");

/// Mirrors `fake_claude.rs`'s own `CALL_LOG_FILENAME`; a bin target exports
/// nothing an external test crate can import, so this is a second literal by
/// necessity (the idiom `materialize_refresh.rs` uses).
const CALL_LOG_FILENAME: &str = "fake-claude-calls.log";

struct Env {
    root: PathBuf,
    cfg: PathBuf,
    data: PathBuf,
    run: PathBuf,
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-heal-needed-e2e-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let env = Env { cfg: root.join("cfg"), data: root.join("data"), run: root.join("run"), path: fixture_path(&bin), root };
        for dir in [&bin, &env.cfg, &env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        stage_double(FAKE_CLAUDE, &bin.join(format!("claude{}", std::env::consts::EXE_SUFFIX)));
        env
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        // `HOME` + `XDG_CONFIG_HOME` under the temp root keep every non-CC
        // backend's config-dir probe false; the curated `PATH` keeps their
        // `which` probe false (the materialize_refresh.rs env).
        cmd.env("CLAUDE_CONFIG_DIR", &self.cfg)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("PATH", &self.path);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// The one line of a multi-agent report that names `agent`.
    fn line_of<'a>(&self, out: &'a str, agent: &str) -> &'a str {
        out.lines().find(|l| l.starts_with(&format!("{agent}: "))).unwrap_or_default()
    }

    /// Every `claude` invocation so far, one per line.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.cfg.join(CALL_LOG_FILENAME)).unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// The single claude stamp marker, rewritten with `reinstalling: true` — the
    /// state a hook killed between `plugin_install` and `record_converged` leaves.
    fn set_marker_reinstalling(&self) {
        let marker = self.marker_path();
        let mut doc: serde_json::Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
        doc["reinstalling"] = serde_json::json!(true);
        fs::write(&marker, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    }

    fn marker_reinstalling(&self) -> bool {
        let doc: serde_json::Value = serde_json::from_slice(&fs::read(self.marker_path()).unwrap()).unwrap();
        doc.get("reinstalling").and_then(serde_json::Value::as_bool).unwrap_or(false)
    }

    fn marker_path(&self) -> PathBuf {
        let markers = self.data.join("ez-fixture-plugin/markers");
        let mut entries = fs::read_dir(&markers).unwrap();
        let path = entries.next().unwrap().unwrap().path();
        assert!(entries.next().is_none(), "expected exactly one stamp marker after a claude-only setup");
        path
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// `PATH` holding the scratch `claude` double and the fixture binary's dir, and
/// nothing else.
fn fixture_path(bin: &Path) -> OsString {
    let fixture_dir = Path::new(BIN).parent().map(Path::to_path_buf).unwrap_or_default();
    std::env::join_paths([bin.to_path_buf(), fixture_dir]).unwrap()
}

/// Stages the CLI double onto the scratch `PATH`: a symlink on unix, a copy
/// elsewhere (a copy write-opens the destination and races a sibling test's
/// exec of it).
#[cfg(unix)]
fn stage_double(src: &str, dst: &Path) {
    std::os::unix::fs::symlink(src, dst).unwrap();
}

#[cfg(not(unix))]
fn stage_double(src: &str, dst: &Path) {
    fs::copy(src, dst).unwrap();
}

#[test]
fn heal_needed_spawns_no_claude() {
    let env = Env::new("spawn");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok, "setup failed: {out}");
    let calls_before = env.calls();
    assert!(!calls_before.is_empty(), "setup must have driven fake_claude for this test to mean anything");

    let (ok, out) = env.fixture(&["heal-needed"]);
    assert!(ok, "heal-needed failed: {out}");

    assert_eq!(env.calls(), calls_before, "heal-needed must answer from files alone; it spawned claude:\n{}", env.calls().join("\n"));
    // `fake_claude` writes no CC registry files after `setup`, so the marker is
    // the only record: the classifier answers "stale marker without a registry
    // row". Pin that exact line, not just the absence of a spawn — a regression
    // routing `claude` to the early `Needed`/`Skipped` branch would otherwise pass.
    assert_eq!(env.line_of(&out, "claude"), "claude: needed (stale marker without a registry row)");
}

#[test]
fn self_heal_ends_a_lingering_reinstall_on_a_healthy_row() {
    let env = Env::new("reinstall");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok, "setup failed: {out}");

    env.set_marker_reinstalling();
    assert!(env.marker_reinstalling(), "the fixture must leave `reinstalling: true` set");

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");
    assert_eq!(out, "NoOp", "a healthy install with a lingering reinstall still converges to NoOp, got {out}");
    assert!(!env.marker_reinstalling(), "self-heal must end a lingering reinstall beside a healthy row");
}

#[test]
fn vscode_copilot_skips_at_user_scope_in_heal_needed() {
    let env = Env::new("scope");

    // Pre-create ~/.vscode so the project-scope-only backend detects; the skip
    // gate (not an absent tool) is then what the classifier reports.
    fs::create_dir_all(env.root.join(".vscode")).unwrap();

    let (ok, out) = env.fixture(&["heal-needed"]);
    assert!(ok, "heal-needed failed: {out}");
    assert_eq!(env.line_of(&out, "vscode-copilot"), "vscode-copilot: skipped (no config surface at this scope)");
}
