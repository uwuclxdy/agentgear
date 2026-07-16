//! Hermetic coverage for the setup picker, fully isolated from the developer's
//! real config. No docker, no auth, no harness binary: agentgear only ever writes
//! each backend's own config files, so we drive `multi-installer <subcommand>`
//! against a temp `HOME` (+ XDG dirs, + `CODEX_HOME`) and read the written files
//! back.
//!
//! Two backends are pre-detected (gemini's HOME-based `~/.gemini`, codex's
//! `CODEX_HOME`) so `setup --agent gemini` has a second detected-but-unfiltered
//! backend to prove it never touches: every path a backend touches derives from
//! the redirected env, so proving codex's file stays byte-identical also proves
//! `--agent` really scopes the fan-out to the one backend named.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use agentgear::PluginHost;
use multi_installer::MultiInstaller;

const BIN: &str = env!("CARGO_BIN_EXE_multi-installer");

/// A foreign gemini mcp server + an unrelated top-level key that MUST outlive our
/// install and uninstall untouched.
const SEED_GEMINI: &str = r#"{
  "theme": "dark",
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  }
}
"#;

/// A foreign codex config that `setup --agent gemini` MUST never write to.
const SEED_CODEX: &str = r#"# the user's own codex config
model = "gpt-5.4"

[mcp_servers.theirs]
command = "their-server"
"#;

struct Env {
    root: PathBuf,
    gemini: PathBuf,
    codex: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the example binary's dir, so `which(<harness>)` stays
    /// false for every backend: detection rides purely on the temp config dirs
    /// this env pre-creates.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across
        // every test in this binary, so a second test would otherwise share (and
        // wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-multi-installer-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            gemini: root.join(".gemini"),
            codex: root.join(".codex"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: bin_dir(),
            root,
        };
        fs::create_dir_all(&env.gemini).unwrap();
        fs::create_dir_all(&env.codex).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.gemini.join("settings.json"), SEED_GEMINI).unwrap();
        fs::write(env.codex.join("config.toml"), SEED_CODEX).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("CODEX_HOME", &self.codex)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("PATH", &self.path);
    }

    fn run(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn gemini_settings(&self) -> String {
        fs::read_to_string(self.gemini.join("settings.json")).unwrap()
    }

    fn codex_config(&self) -> String {
        fs::read_to_string(self.codex.join("config.toml")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the example binary's directory: no harness CLIs, so detect()
/// rides purely on the pre-created config dirs regardless of the dev box.
fn bin_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn status_lists_every_agent() {
    let env = Env::new("status");
    let (ok, out) = env.run(&["status"]);
    assert!(ok, "status failed: {out}");

    // Every backend the derive declares gets its own row (the enumeration API
    // this example demonstrates), regardless of whether it is detected here.
    for id in MultiInstaller::AGENTS {
        assert!(out.lines().any(|line| line.split_whitespace().next() == Some(id)), "status is missing a row for `{id}`:\n{out}");
    }

    // The two backends this env pre-created report detected; an arbitrary
    // undetected one (no config dir, no PATH entry) reports not detected.
    let gemini_row = out.lines().find(|l| l.starts_with("gemini ")).expect("gemini row missing");
    assert!(gemini_row.contains("yes"), "gemini should be detected: {gemini_row}");
    let codex_row = out.lines().find(|l| l.starts_with("codex ")).expect("codex row missing");
    assert!(codex_row.contains("yes"), "codex should be detected: {codex_row}");
    let zed_row = out.lines().find(|l| l.starts_with("zed ")).expect("zed row missing");
    assert!(zed_row.contains(" no "), "zed should not be detected in a fresh env: {zed_row}");
}

#[test]
fn setup_agent_filter_scopes_to_one_backend() {
    let env = Env::new("setup");
    let cmd_dir = env.gemini.join("commands").join("multi-installer");

    // install: --agent gemini touches only gemini, though codex is also detected.
    let (ok, out) = env.run(&["setup", "--agent", "gemini"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let s = env.gemini_settings();
    assert!(s.contains("multi-installer"), "our mcp server key missing:\n{s}");
    assert!(s.contains("theirs") && s.contains("their-server"), "seeded gemini mcp server was clobbered:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("dark"), "seeded gemini top-level key was clobbered:\n{s}");

    // codex is a detected, unfiltered-out backend that never received `--agent`:
    // its config must stay byte-identical, proving the filter is exact.
    assert_eq!(env.codex_config(), SEED_CODEX, "setup --agent gemini touched codex's config");

    assert!(cmd_dir.exists(), "gemini command dir not written: {}", cmd_dir.display());
    for p in [env.gemini.join("settings.json"), cmd_dir.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.run(&["setup", "--agent", "gemini"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");
    assert_eq!(env.codex_config(), SEED_CODEX, "second setup touched codex's config");

    // uninstall (no --agent filter, matching kitchen-sink/host-fixture): removes
    // our gemini entries, leaves the seeded gemini AND codex config untouched.
    let (ok, out) = env.run(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let s = env.gemini_settings();
    assert!(!s.contains("multi-installer"), "our mcp server survived uninstall:\n{s}");
    assert!(s.contains("theirs") && s.contains("their-server"), "uninstall removed the seeded gemini mcp server:\n{s}");
    assert!(!cmd_dir.exists(), "our command dir survived uninstall: {}", cmd_dir.display());
    assert_eq!(env.codex_config(), SEED_CODEX, "uninstall touched codex's config");
}
