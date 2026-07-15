//! Hermetic lifecycle coverage for the kitchen-sink example, fully isolated from the
//! developer's real config. No docker, no auth, no harness binary: agentgear only ever
//! writes each harness's config files, so we drive `kitchen-sink setup --agent <id>`
//! against a temp `HOME` (+ XDG dirs) and read the written files back.
//!
//! Two backends are exercised: gemini (HOME-based `~/.gemini`, JSON `mcpServers` +
//! hooks + a command TOML) and crush (`$XDG_CONFIG_HOME/crush`, JSON `mcp` map; its
//! only hook event is `PreToolUse`, which this plugin does not use, so crush gets the
//! MCP server alone). Every path a backend touches derives from the redirected env, so
//! proving our files land under the temp root also proves nothing reaches the real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_kitchen-sink");

/// A foreign gemini mcp server + an unrelated top-level key that MUST outlive our
/// install and uninstall untouched.
const SEED_GEMINI: &str = r#"{
  "theme": "dark",
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  }
}
"#;

/// A foreign crush mcp server + an unrelated top-level key that MUST survive.
const SEED_CRUSH: &str = r#"{
  "options": { "theme": "dark" },
  "mcp": {
    "theirs": { "type": "stdio", "command": "their-server" }
  }
}
"#;

struct Env {
    root: PathBuf,
    gemini: PathBuf,
    crush: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the example binary's dir, so `which(<harness>)` (and `claude`)
    /// stays false: detection rides purely on the temp config dirs we pre-create.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-kitchen-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let config = root.join("config");
        Env {
            gemini: root.join(".gemini"),
            crush: config.join("crush"),
            config,
            data: root.join("data"),
            run: root.join("run"),
            path: bin_dir(),
            root,
        }
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
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
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the example binary's directory: no harness CLIs, no `claude`, so the
/// fan-out stays a pure single-backend exercise regardless of the dev box.
fn bin_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn gemini_lifecycle() {
    let env = Env::new("gemini");
    // Pre-create ~/.gemini so detect() passes with no `gemini` on PATH, and seed an
    // unrelated user config the lifecycle must preserve.
    fs::create_dir_all(&env.gemini).unwrap();
    for dir in [&env.data, &env.run] {
        fs::create_dir_all(dir).unwrap();
    }
    let settings = env.gemini.join("settings.json");
    fs::write(&settings, SEED_GEMINI).unwrap();
    let cmd_dir = env.gemini.join("commands").join("kitchen-sink");
    let cmd_file = cmd_dir.join("greet.toml");

    // install: mcp + hooks + command translated into gemini's config.
    let (ok, out) = env.run(&["setup", "--agent", "gemini"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let s = fs::read_to_string(&settings).unwrap();
    assert!(s.contains("kitchen-sink"), "our mcp server key missing:\n{s}");
    // hooks: SessionStart identity + UserPromptSubmit -> gemini's BeforeAgent.
    assert!(s.contains("SessionStart"), "SessionStart hook missing:\n{s}");
    assert!(s.contains("BeforeAgent"), "UserPromptSubmit was not mapped to BeforeAgent:\n{s}");
    assert!(s.contains("self-heal"), "SessionStart hook command missing:\n{s}");
    assert!(s.contains("check-restart"), "UserPromptSubmit hook command missing:\n{s}");
    // the seeded user config survived the merge.
    assert!(s.contains("theirs") && s.contains("their-server"), "seeded mcp server was clobbered:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("dark"), "seeded top-level key was clobbered:\n{s}");

    // command: one namespaced TOML with the frontmatter description + body prompt.
    assert!(cmd_file.exists(), "command TOML not written: {}", cmd_file.display());
    let c = fs::read_to_string(&cmd_file).unwrap();
    assert!(c.contains("prompt ="), "command TOML missing prompt:\n{c}");
    assert!(c.contains("Greet the user"), "command body not translated to prompt:\n{c}");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [settings.clone(), cmd_file.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.run(&["setup", "--agent", "gemini"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: ours gone, the user's kept.
    let (ok, out) = env.run(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let s = fs::read_to_string(&settings).unwrap();
    assert!(!s.contains("self-heal") && !s.contains("check-restart"), "our hooks survived uninstall:\n{s}");
    assert!(s.contains("theirs") && s.contains("their-server"), "uninstall removed the seeded mcp server:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("dark"), "uninstall removed the seeded top-level key:\n{s}");
    assert!(!cmd_dir.exists(), "our command dir survived uninstall: {}", cmd_dir.display());

    // the post-uninstall config still parses: a clean re-install lands again.
    let (ok, out) = env.run(&["setup", "--agent", "gemini"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(fs::read_to_string(&settings).unwrap().contains("kitchen-sink"), "re-install did not re-add our server");
}

#[test]
fn crush_lifecycle() {
    let env = Env::new("crush");
    // Pre-create $XDG_CONFIG_HOME/crush so detect() passes with no `crush` on PATH.
    fs::create_dir_all(&env.crush).unwrap();
    for dir in [&env.data, &env.run] {
        fs::create_dir_all(dir).unwrap();
    }
    let config = env.crush.join("crush.json");
    fs::write(&config, SEED_CRUSH).unwrap();

    // install: crush maps only PreToolUse hooks (none here), so it gets the mcp server.
    let (ok, out) = env.run(&["setup", "--agent", "crush"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let c = fs::read_to_string(&config).unwrap();
    assert!(c.contains("kitchen-sink"), "our mcp server missing from crush.json:\n{c}");
    assert!(c.contains("theirs") && c.contains("their-server"), "seeded mcp server was clobbered:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "seeded top-level key was clobbered:\n{c}");
    assert!(config.starts_with(&env.root), "backend wrote outside the temp root: {}", config.display());

    // idempotent: a second reconcile is a true NoOp.
    let (ok, out) = env.run(&["setup", "--agent", "crush"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: ours gone, the user's kept.
    let (ok, out) = env.run(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let c = fs::read_to_string(&config).unwrap();
    assert!(!c.contains("kitchen-sink"), "our mcp server survived uninstall:\n{c}");
    assert!(c.contains("theirs") && c.contains("their-server"), "uninstall removed the seeded mcp server:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "uninstall removed the seeded top-level key:\n{c}");
}
