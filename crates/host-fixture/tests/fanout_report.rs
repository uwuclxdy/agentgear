//! Hermetic pin of the per-agent fan-out report (`setup-report` = the fixture's
//! `install_into_report` + `AgentReport` Display): a detected backend reads as
//! its own outcome, an undetected one as an explicit skip, and an `--agent`
//! filter drops filtered-out agents from the report entirely. Same temp-`HOME`
//! isolation as the per-backend hermetic tests (gemini is the detected backend
//! because pre-creating `~/.gemini` is all its `detect()` needs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");
const GITHUB_BIN: &str = env!("CARGO_BIN_EXE_github_fixture");

struct Env {
    root: PathBuf,
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-fanout-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env { path: fixture_dir(), root };
        // Only gemini's detection dir exists, so every other backend must land
        // in the report as a skip, not silently vanish.
        fs::create_dir_all(env.root.join(".gemini")).unwrap();
        for dir in ["config", "data", "run"] {
            fs::create_dir_all(env.root.join(dir)).unwrap();
        }
        env
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        self.run(BIN, args)
    }

    fn run(&self, bin: &str, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("PATH", &self.path);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH holding only the fixture binary's dir, so no backend detects via `which`.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn report_carries_one_line_per_agent_with_skips_visible() {
    let env = Env::new("full");

    // Filtered: only the asked-for agent appears, with its own outcome.
    let (ok, out) = env.fixture(&["setup-report", "--agent", "gemini"]);
    assert!(ok, "filtered setup-report failed: {out}");
    assert_eq!(out, "gemini: installed", "filtered report should be exactly the one agent");

    // A second converge is a per-agent no-op, not a silent line drop.
    let (ok, out) = env.fixture(&["setup-report", "--agent", "gemini"]);
    assert!(ok && out == "gemini: no changes needed", "second setup-report should no-op, got {out}");

    // Unfiltered: every one of the fixture's 25 agents gets a line — converged
    // for the detected one, an explicit skip for the rest.
    let (ok, out) = env.fixture(&["setup-report"]);
    assert!(ok, "unfiltered setup-report failed: {out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 25, "one line per configured agent:\n{out}");
    assert!(lines.contains(&"gemini: no changes needed"), "gemini line missing:\n{out}");
    assert!(lines.contains(&"zed: skipped (not installed on this machine)"), "undetected zed should read as a skip:\n{out}");
    assert!(lines.contains(&"claude: skipped (not installed on this machine)"), "claude (not on PATH here) should read as a skip:\n{out}");
}

/// One bad agent must not nuke the fan-out: gemini (earlier in the agents list)
/// is broken — its seeded `settings.json` is unparseable, so its reconcile
/// refuses to clobber it — yet droid (later in the list) still installs,
/// self-heals, and uninstalls. Before the isolation fix, gemini's error aborted
/// every one of these calls mid-fan-out, stranding droid.
#[test]
fn a_failing_agent_does_not_strand_the_rest_of_the_fanout() {
    let env = Env::new("broken");
    fs::write(env.root.join(".gemini").join("settings.json"), "{ this is not json").unwrap();
    fs::create_dir_all(env.root.join(".factory")).unwrap();
    let droid_mcp = env.root.join(".factory").join("mcp.json");

    // install: gemini fails, droid (after it) still converges.
    let (ok, out) = env.fixture(&["setup-report", "--agent", "gemini", "--agent", "droid"]);
    assert!(!ok, "a failed agent must flip the exit code:\n{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.iter().any(|l| l.starts_with("gemini: failed: ")), "gemini must read as failed:\n{out}");
    assert!(lines.contains(&"droid: installed"), "droid must install despite gemini failing first:\n{out}");
    assert!(droid_mcp.exists(), "droid's mcp.json must have been written");

    // the legacy merged path also finishes the fan-out before reporting the failure.
    let (ok, _) = env.fixture(&["setup", "--agent", "gemini", "--agent", "droid"]);
    assert!(!ok, "legacy setup must still surface the failure via its exit code");

    // self_heal: gemini's probe fails on the same unparseable config; droid heals to a no-op.
    let (ok, out) = env.fixture(&["self-heal-report"]);
    assert!(!ok, "self-heal must surface gemini's failure:\n{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.iter().any(|l| l.starts_with("gemini: failed: ")), "gemini must read as failed:\n{out}");
    assert!(lines.contains(&"droid: no changes needed"), "droid must still heal cleanly:\n{out}");

    // uninstall: gemini's remove fails, droid's entries still come out.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(!ok, "uninstall must surface gemini's failure: {out}");
    // Our entry was all this mcp.json ever held, so taking it back takes the file.
    assert!(
        !droid_mcp.exists(),
        "droid's mcp entry must be removed despite gemini failing first:\n{}",
        fs::read_to_string(&droid_mcp).unwrap()
    );
}

/// A github-source host with a config-merge backend in `agents = [...]`: the
/// backend has no local tree to render from, so it must be a VISIBLE skip (and
/// a doctor Warn) — never a mid-fan-out error after earlier agents already
/// wrote, and never a silent drop. Driven through `github_fixture`
/// (`default_source = "github"`, zero-embed); nothing here reaches the network,
/// because the only github-capable backend (claude) is undetected in this env
/// and skipped agents never touch the source.
#[test]
fn github_source_skips_config_backends_visibly() {
    let env = Env::new("github");
    let settings = env.root.join(".gemini").join("settings.json");

    let (ok, out) = env.run(GITHUB_BIN, &["setup-report"]);
    assert!(ok, "a source skip is not a failure: {out}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines.contains(&"gemini: skipped (cannot serve a github source; use an embedded or path source)"),
        "gemini must skip visibly:\n{out}"
    );
    assert!(lines.contains(&"claude: skipped (not installed on this machine)"), "claude line missing:\n{out}");
    assert!(!settings.exists(), "a skipped backend must write nothing");

    let (ok, out) = env.run(GITHUB_BIN, &["doctor"]);
    assert!(ok, "the skip must be a Warn, not a Fail: {out}");
    assert!(out.lines().any(|l| l.starts_with("[warn] gemini: github source")), "doctor must warn about the github-source skip:\n{out}");
    assert!(!out.contains("[fail]"), "no check may fail on a healthy github-source host:\n{out}");
}

/// Uninstall on a zero-embed github host with a DETECTED config backend that was
/// never installed: the backend has no local tree to render a strip-set from, so
/// it must skip visibly — never `failed: invalid plugin tree: embedded blob is
/// empty` (which told the user to stop using the source they are already on) —
/// and the legacy merged `uninstall()` must stay `Ok`.
#[test]
fn zero_embed_github_uninstall_skips_config_backends() {
    let env = Env::new("gh-uninstall");

    let (ok, out) = env.run(GITHUB_BIN, &["uninstall-report"]);
    assert!(ok, "a source skip is not a failure: {out}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines.contains(&"gemini: skipped (cannot serve a github source; use an embedded or path source)"),
        "gemini must skip visibly:\n{out}"
    );
    assert!(lines.contains(&"claude: skipped (not installed on this machine)"), "claude line missing:\n{out}");

    let (ok, out) = env.run(GITHUB_BIN, &["uninstall"]);
    assert!(ok, "legacy uninstall must collapse a source skip to Ok, got: {out}");
    assert_eq!(out, "NoOp", "nothing was installed, so nothing changed: {out}");
}

/// The other two lifecycle entry points against the same zero-embed github
/// host: `self_heal` (the SessionStart entrypoint) and `update` must exhibit the
/// same visible-skip contract as `setup`/`uninstall` above — never a mid-fan-out
/// error, never a silent drop, and the legacy merged path collapses to `Ok`.
/// Nothing was ever installed, so both report/legacy pairs converge on the same
/// skip lines and a `NoOp` merge.
#[test]
fn github_source_skips_config_backends_visibly_on_self_heal_and_update() {
    let env = Env::new("gh-heal-update");
    let settings = env.root.join(".gemini").join("settings.json");

    let (ok, out) = env.run(GITHUB_BIN, &["self-heal-report"]);
    assert!(ok, "a source skip is not a failure: {out}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines.contains(&"gemini: skipped (cannot serve a github source; use an embedded or path source)"),
        "gemini must skip visibly:\n{out}"
    );
    assert!(lines.contains(&"claude: skipped (not installed on this machine)"), "claude line missing:\n{out}");
    assert!(!settings.exists(), "a skipped backend must write nothing");

    let (ok, out) = env.run(GITHUB_BIN, &["self-heal"]);
    assert!(ok, "legacy self_heal must collapse a source skip to Ok, got: {out}");
    assert_eq!(out, "NoOp", "nothing was installed, so nothing changed: {out}");

    let (ok, out) = env.run(GITHUB_BIN, &["update-report"]);
    assert!(ok, "a source skip is not a failure: {out}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines.contains(&"gemini: skipped (cannot serve a github source; use an embedded or path source)"),
        "gemini must skip visibly:\n{out}"
    );
    assert!(lines.contains(&"claude: skipped (not installed on this machine)"), "claude line missing:\n{out}");
    assert!(!settings.exists(), "a skipped backend must write nothing");

    let (ok, out) = env.run(GITHUB_BIN, &["update"]);
    assert!(ok, "legacy update must collapse a source skip to Ok, got: {out}");
    assert_eq!(out, "NoOp", "nothing was installed, so nothing changed: {out}");
}
