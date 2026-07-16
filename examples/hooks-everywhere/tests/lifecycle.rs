//! Hermetic per-backend hook-landing coverage for hooks-everywhere, fully isolated
//! from the developer's real config. No docker, no auth, no harness binary:
//! agentgear only ever writes each harness's config files, so we drive
//! `hooks-everywhere setup --agent <id>` against a temp `HOME` (+ per-backend
//! overrides) and read the written files back.
//!
//! Five representative harnesses, chosen to cover both ends of hook translation:
//! - codex, gemini, kimi, droid: all four CC events (`SessionStart`,
//!   `UserPromptSubmit`, `PreToolUse`, `PostToolUse`) have a mapped analog, so
//!   every hook lands (each backend in its own config-file shape: bespoke toml,
//!   one JSON settings file, a `[[hooks]]` toml array, and a dedicated hooks.json).
//! - crush: defines exactly one hook event (`PreToolUse`) — proving the other
//!   three are skipped, not silently mis-mapped, is the point of this harness.
//!
//! Every path a backend touches derives from the redirected env, so proving our
//! files land under the temp root (and a pre-seeded foreign hook survives both
//! install and uninstall) also proves nothing reaches the real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_hooks-everywhere");

struct Env {
    root: PathBuf,
    /// `<root>/.codex`, pointed at via `CODEX_HOME`.
    codex: PathBuf,
    /// `<root>/.gemini`, HOME-based (gemini has no dedicated override env).
    gemini: PathBuf,
    /// `<root>/custom-kimi-home`, pointed at via `KIMI_CODE_HOME`. Deliberately
    /// NOT `<root>/.kimi-code` (the HOME-based fallback), so a regression that
    /// dropped the override can't silently alias onto the fallback and still pass.
    kimi: PathBuf,
    /// `<config>/crush`, pointed at via `CRUSH_GLOBAL_CONFIG`.
    crush: PathBuf,
    /// `<root>/.factory`, HOME-based (droid has no dedicated override env either).
    factory: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the example binary's dir, so `which(<harness>)` stays
    /// false and detection rides purely on the pre-created config dirs.
    path: OsString,
}

impl Env {
    /// `name` disambiguates the temp root: `process::id()` is constant across every
    /// test in this binary, so two tests would otherwise share (and wipe) one root.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-hooks-everywhere-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let config = root.join("config");
        let env = Env {
            codex: root.join(".codex"),
            gemini: root.join(".gemini"),
            kimi: root.join("custom-kimi-home"),
            crush: config.join("crush"),
            factory: root.join(".factory"),
            data: root.join("data"),
            run: root.join("run"),
            path: bin_dir(),
            config,
            root,
        };
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("CODEX_HOME", &self.codex)
            .env("KIMI_CODE_HOME", &self.kimi)
            .env("CRUSH_GLOBAL_CONFIG", &self.crush)
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

/// PATH with only the example binary's directory: no harness CLIs, no `claude`, so
/// the fan-out stays a pure single-backend exercise regardless of the dev box.
fn bin_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

/// A foreign hook under an event we also write to (`SessionStart`, collides with
/// ours) plus one under an event we never touch (`Stop`), both of which MUST
/// outlive our install and uninstall.
const SEED_CODEX_HOOKS: &str = r#"{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "their-session-hook" } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "command": "their-stop-hook" } ] }
    ]
  }
}
"#;

#[test]
fn codex_all_events_land() {
    let env = Env::new("codex");
    fs::create_dir_all(&env.codex).unwrap();
    let hooks_file = env.codex.join("hooks.json");
    fs::write(&hooks_file, SEED_CODEX_HOOKS).unwrap();

    let (ok, out) = env.run(&["setup", "--agent", "codex"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let h = fs::read_to_string(&hooks_file).unwrap();
    // codex mirrors CC event names 1:1: all four of our bindings land.
    assert!(h.contains("hooks-everywhere self-heal"), "SessionStart hook missing:\n{h}");
    assert!(h.contains("hooks-everywhere check-restart"), "UserPromptSubmit hook missing:\n{h}");
    assert!(h.contains("hooks-everywhere guard"), "PreToolUse hook missing:\n{h}");
    assert!(h.contains("hooks-everywhere audit"), "PostToolUse hook missing:\n{h}");
    assert!(h.contains("\"matcher\": \"Bash\""), "PreToolUse matcher missing:\n{h}");
    // the seeded foreign hooks survived the merge: one under a colliding event,
    // one under an event we never touch.
    assert!(h.contains("their-session-hook"), "seeded SessionStart hook was clobbered:\n{h}");
    assert!(h.contains("their-stop-hook"), "seeded Stop hook was clobbered:\n{h}");
    assert!(hooks_file.starts_with(&env.root), "backend wrote outside the temp root: {}", hooks_file.display());

    let (ok, out) = env.run(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let h = fs::read_to_string(&hooks_file).unwrap();
    assert!(!h.contains("hooks-everywhere self-heal") && !h.contains("hooks-everywhere audit"), "our hooks survived uninstall:\n{h}");
    assert!(h.contains("their-session-hook"), "uninstall removed the seeded SessionStart hook:\n{h}");
    assert!(h.contains("their-stop-hook"), "uninstall removed the seeded Stop hook:\n{h}");
}

/// A foreign `SessionStart` group (collides with ours) plus an unrelated top-level
/// key, both of which MUST outlive our install and uninstall.
const SEED_GEMINI_SETTINGS: &str = r#"{
  "theme": "dark",
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "their-startup-hook.sh" } ] }
    ]
  }
}
"#;

#[test]
fn gemini_all_events_land() {
    let env = Env::new("gemini");
    fs::create_dir_all(&env.gemini).unwrap();
    let settings = env.gemini.join("settings.json");
    fs::write(&settings, SEED_GEMINI_SETTINGS).unwrap();

    let (ok, out) = env.run(&["setup", "--agent", "gemini"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let s = fs::read_to_string(&settings).unwrap();
    // gemini's own event names: SessionStart identity, UserPromptSubmit ->
    // BeforeAgent, PreToolUse -> BeforeTool, PostToolUse -> AfterTool.
    assert!(s.contains("\"SessionStart\""), "SessionStart hook missing:\n{s}");
    assert!(s.contains("\"BeforeAgent\""), "UserPromptSubmit was not mapped to BeforeAgent:\n{s}");
    assert!(s.contains("\"BeforeTool\""), "PreToolUse was not mapped to BeforeTool:\n{s}");
    assert!(s.contains("\"AfterTool\""), "PostToolUse was not mapped to AfterTool:\n{s}");
    assert!(s.contains("hooks-everywhere self-heal"), "SessionStart hook command missing:\n{s}");
    assert!(s.contains("hooks-everywhere check-restart"), "BeforeAgent hook command missing:\n{s}");
    assert!(s.contains("hooks-everywhere guard"), "BeforeTool hook command missing:\n{s}");
    assert!(s.contains("hooks-everywhere audit"), "AfterTool hook command missing:\n{s}");
    assert!(s.contains("\"matcher\": \"Bash\""), "BeforeTool matcher missing:\n{s}");
    // the seeded user config survived the merge.
    assert!(s.contains("their-startup-hook.sh"), "seeded SessionStart hook was clobbered:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("dark"), "seeded top-level key was clobbered:\n{s}");
    assert!(settings.starts_with(&env.root), "backend wrote outside the temp root: {}", settings.display());

    let (ok, out) = env.run(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let s = fs::read_to_string(&settings).unwrap();
    assert!(!s.contains("hooks-everywhere self-heal") && !s.contains("hooks-everywhere audit"), "our hooks survived uninstall:\n{s}");
    assert!(s.contains("their-startup-hook.sh"), "uninstall removed the seeded SessionStart hook:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("dark"), "uninstall removed the seeded top-level key:\n{s}");
}

/// A foreign `SessionStart` hook (collides), a foreign `Stop` hook (untouched
/// event), an unrelated top-level key, and a comment — all of which MUST outlive
/// our install and uninstall. `toml_edit` is what preserves the comment/key order,
/// so this seed doubles as a check that we never fall back to a naive re-serialize.
const SEED_KIMI_CONFIG: &str = r#"# the user's own kimi config
model = "kimi-k2"

[[hooks]]
event = "SessionStart"
command = "their-session-hook"

[[hooks]]
event = "Stop"
command = "their-stop-hook"
"#;

#[test]
fn kimi_all_events_land() {
    let env = Env::new("kimi");
    fs::create_dir_all(&env.kimi).unwrap();
    let config = env.kimi.join("config.toml");
    fs::write(&config, SEED_KIMI_CONFIG).unwrap();

    let (ok, out) = env.run(&["setup", "--agent", "kimi"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let c = fs::read_to_string(&config).unwrap();
    // kimi mirrors CC event names 1:1 as `[[hooks]]` tables: all four land.
    assert!(c.contains("event = \"SessionStart\""), "SessionStart hook missing:\n{c}");
    assert!(c.contains("event = \"UserPromptSubmit\""), "UserPromptSubmit hook missing:\n{c}");
    assert!(c.contains("event = \"PreToolUse\""), "PreToolUse hook missing:\n{c}");
    assert!(c.contains("event = \"PostToolUse\""), "PostToolUse hook missing:\n{c}");
    assert!(c.contains("hooks-everywhere self-heal"), "SessionStart hook command missing:\n{c}");
    assert!(c.contains("hooks-everywhere check-restart"), "UserPromptSubmit hook command missing:\n{c}");
    assert!(c.contains("hooks-everywhere guard"), "PreToolUse hook command missing:\n{c}");
    assert!(c.contains("hooks-everywhere audit"), "PostToolUse hook command missing:\n{c}");
    assert!(c.contains("matcher = \"Bash\""), "PreToolUse matcher missing:\n{c}");
    // the seeded user config survived: colliding-event hook, untouched-event hook,
    // top-level key, and comment.
    assert!(c.contains("their-session-hook"), "seeded SessionStart hook was clobbered:\n{c}");
    assert!(c.contains("their-stop-hook"), "seeded Stop hook was clobbered:\n{c}");
    assert!(c.contains("model = \"kimi-k2\""), "seeded top-level key was clobbered:\n{c}");
    assert!(c.contains("the user's own kimi config"), "seeded comment was dropped (naive re-serialize?):\n{c}");
    assert!(config.starts_with(&env.root), "backend wrote outside the temp root: {}", config.display());

    let (ok, out) = env.run(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let c = fs::read_to_string(&config).unwrap();
    assert!(!c.contains("hooks-everywhere self-heal") && !c.contains("hooks-everywhere audit"), "our hooks survived uninstall:\n{c}");
    assert!(c.contains("their-session-hook"), "uninstall removed the seeded SessionStart hook:\n{c}");
    assert!(c.contains("their-stop-hook"), "uninstall removed the seeded Stop hook:\n{c}");
    assert!(c.contains("model = \"kimi-k2\""), "uninstall removed the seeded top-level key:\n{c}");
    assert!(c.contains("the user's own kimi config"), "uninstall dropped the seeded comment:\n{c}");
}

/// A user's own `SessionStart` hook (collides with ours), living in droid's
/// dedicated `hooks.json`, that MUST outlive our install and uninstall.
const SEED_DROID_HOOKS: &str = r#"{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "their-startup-hook.sh" } ] }
    ]
  }
}
"#;

#[test]
fn droid_all_events_land() {
    let env = Env::new("droid");
    fs::create_dir_all(&env.factory).unwrap();
    let hooks_file = env.factory.join("hooks.json");
    fs::write(&hooks_file, SEED_DROID_HOOKS).unwrap();

    let (ok, out) = env.run(&["setup", "--agent", "droid"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let h = fs::read_to_string(&hooks_file).unwrap();
    // droid mirrors CC event names 1:1: all four of our bindings land.
    assert!(h.contains("hooks-everywhere self-heal"), "SessionStart hook missing:\n{h}");
    assert!(h.contains("hooks-everywhere check-restart"), "UserPromptSubmit hook missing:\n{h}");
    assert!(h.contains("hooks-everywhere guard"), "PreToolUse hook missing:\n{h}");
    assert!(h.contains("hooks-everywhere audit"), "PostToolUse hook missing:\n{h}");
    assert!(h.contains("\"matcher\": \"Bash\""), "PreToolUse matcher missing:\n{h}");
    // the seeded foreign hook survived the merge.
    assert!(h.contains("their-startup-hook.sh"), "seeded SessionStart hook was clobbered:\n{h}");
    assert!(hooks_file.starts_with(&env.root), "backend wrote outside the temp root: {}", hooks_file.display());

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.run(&["setup", "--agent", "droid"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    let (ok, out) = env.run(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let h = fs::read_to_string(&hooks_file).unwrap();
    assert!(!h.contains("hooks-everywhere self-heal") && !h.contains("hooks-everywhere audit"), "our hooks survived uninstall:\n{h}");
    assert!(h.contains("their-startup-hook.sh"), "uninstall removed the seeded SessionStart hook:\n{h}");
}

/// A foreign `PreToolUse` hook (collides with our only landing event) plus an
/// unrelated top-level key, both of which MUST outlive our install and uninstall.
const SEED_CRUSH_CONFIG: &str = r#"{
  "theme": "dark",
  "hooks": {
    "PreToolUse": [
      { "command": "their-guard.sh" }
    ]
  }
}
"#;

/// Crush defines exactly one hook event (`PreToolUse`); the other three of our
/// bindings have no crush analog and must be skipped, not written under a guess.
#[test]
fn crush_only_pretooluse_lands() {
    let env = Env::new("crush");
    fs::create_dir_all(&env.crush).unwrap();
    let config = env.crush.join("crush.json");
    fs::write(&config, SEED_CRUSH_CONFIG).unwrap();

    let (ok, out) = env.run(&["setup", "--agent", "crush"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let c = fs::read_to_string(&config).unwrap();
    assert!(c.contains("hooks-everywhere guard"), "PreToolUse hook (guard) missing:\n{c}");
    assert!(c.contains("\"matcher\": \"Bash\""), "PreToolUse matcher missing:\n{c}");
    // the three events crush does not define never land, at all.
    assert!(!c.contains("hooks-everywhere self-heal"), "SessionStart has no crush analog, should be absent:\n{c}");
    assert!(!c.contains("hooks-everywhere check-restart"), "UserPromptSubmit has no crush analog, should be absent:\n{c}");
    assert!(!c.contains("hooks-everywhere audit"), "PostToolUse has no crush analog, should be absent:\n{c}");
    // the seeded user config survived the merge.
    assert!(c.contains("their-guard.sh"), "seeded PreToolUse hook was clobbered:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "seeded top-level key was clobbered:\n{c}");
    assert!(config.starts_with(&env.root), "backend wrote outside the temp root: {}", config.display());

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.run(&["setup", "--agent", "crush"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    let (ok, out) = env.run(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let c = fs::read_to_string(&config).unwrap();
    assert!(!c.contains("hooks-everywhere guard"), "our PreToolUse hook survived uninstall:\n{c}");
    assert!(c.contains("their-guard.sh"), "uninstall removed the seeded PreToolUse hook:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "uninstall removed the seeded top-level key:\n{c}");
}
