//! Pins `fake_claude`'s `CLAUDE_CONFIG_DIR` resolution end to end, by spawning the
//! actual binary instead of unit-testing the pure resolver it's built on
//! (`fake_claude.rs`'s own `resolve_config_dir` tests). Unit-testing the resolver
//! alone leaves the real call path — the env-var name and the filename join inside
//! `state_path()` — unpinned: someone could refilter an empty `CLAUDE_CONFIG_DIR` back
//! onto a home fallback there and every resolver test would stay green.
//!
//! `HOME` is redirected to a scratch dir in both cases so a failure can never touch
//! the real one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const FAKE_CLAUDE: &str = env!("CARGO_BIN_EXE_fake_claude");

/// Mirrors `fake_claude.rs`'s own `STATE_FILENAME`; a bin target exports nothing an
/// external test crate can import, so this is a second literal by necessity, not by
/// oversight.
const STATE_FILENAME: &str = "fake-claude-state.json";

/// A scratch root with `cwd` and `home` as separate subdirs, so a fallback-to-home
/// bug and a fallback-to-cwd bug can never land on the same path by coincidence.
struct Scratch {
    root: PathBuf,
    cwd: PathBuf,
    home: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("fake-claude-config-dir-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cwd = root.join("cwd");
        let home = root.join("home");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&home).unwrap();
        Self { root, cwd, home }
    }

    /// Runs `fake_claude plugin marketplace add <name>` — the cheapest subcommand that
    /// reaches `State::save()` without needing a real marketplace tree on disk (the
    /// double's `marketplace_entry` only stats the path to look for a manifest; a
    /// missing one falls back to treating it as a bare name).
    fn run(&self, config_dir: &str) -> bool {
        Command::new(FAKE_CLAUDE)
            .args(["plugin", "marketplace", "add", "test-marketplace"])
            .current_dir(&self.cwd)
            .env("CLAUDE_CONFIG_DIR", config_dir)
            .env("HOME", &self.home)
            .output()
            .unwrap()
            .status
            .success()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn empty_config_dir_writes_state_next_to_cwd_not_home() {
    let scratch = Scratch::new("empty");
    assert!(scratch.run(""));

    assert!(scratch.cwd.join(STATE_FILENAME).is_file(), "state file should land next to the scratch cwd");
    assert!(!scratch.home.join(".claude").exists(), "an empty CLAUDE_CONFIG_DIR must never fall back to $HOME/.claude");
}

#[test]
fn explicit_config_dir_writes_state_there() {
    let scratch = Scratch::new("explicit");
    let explicit = scratch.root.join("explicit-config");
    assert!(scratch.run(explicit.to_str().unwrap()));

    assert!(explicit.join(STATE_FILENAME).is_file(), "state file should land in the explicit config dir");
    assert!(!scratch.cwd.join(STATE_FILENAME).exists());
    assert!(!scratch.home.join(".claude").exists());
}
