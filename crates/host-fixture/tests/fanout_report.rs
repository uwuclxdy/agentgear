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
        let mut cmd = Command::new(BIN);
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
