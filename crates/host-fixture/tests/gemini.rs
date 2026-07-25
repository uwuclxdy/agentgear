//! Hermetic gemini-backend lifecycle, fully isolated from the real `~/.gemini`.
//! No docker, no auth, no `gemini` binary: the backend only ever writes gemini's
//! config files, so we drive `host_fixture setup --agent gemini` against a temp
//! `HOME` (+ XDG dirs) and assert the written `settings.json` / command TOML /
//! agent markdown by parsing them back. `detect()` passes off the pre-created
//! `~/.gemini` dir alone.
//!
//! Every path the backend touches derives from `HOME`, which we point at a throwaway
//! temp root — so proving our files land under that root (and the seeded user
//! entries survive) also proves the backend never reaches the developer's real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our
/// install and uninstall untouched.
const SEED_SETTINGS: &str = r#"{
  "theme": "dark",
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  }
}
"#;

struct Env {
    root: PathBuf,
    gemini: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("gemini")` (and every
    /// other backend's PATH probe) stays false and detection rides on `~/.gemini`.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-gemini-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            gemini: root.join(".gemini"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create ~/.gemini so detect() passes with no `gemini` on PATH, and seed
        // an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.gemini).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.gemini.join("settings.json"), SEED_SETTINGS).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", &self.config)
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

    fn settings(&self) -> String {
        fs::read_to_string(self.gemini.join("settings.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `gemini`, no sibling agent
/// CLIs, so the fan-out stays a pure gemini exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn gemini_full_lifecycle() {
    let env = Env::new("lifecycle");
    let cmd_dir = env.gemini.join("commands").join("ez-fixture-plugin");
    let cmd_file = cmd_dir.join("hello.toml");
    let agent_file = env.gemini.join("agents").join("ez-fixture-plugin-ez-helper.md");

    // install: translates mcp + hooks + commands + agents into gemini's config.
    let (ok, out) = env.fixture(&["setup", "--agent", "gemini"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // A fresh, healthy install must self-heal to a true NoOp: probe reads every surface
    // reconcile just wrote and finds no drift. Guards against a probe/reconcile desync
    // (widened surface probe, or probe rendering from the wrong source) that would churn.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    let s = env.settings();
    // our mcp server landed under `mcpServers`, Plain shape.
    assert!(s.contains("ez-fixture"), "our mcp server key missing:\n{s}");
    assert!(s.contains("host_fixture"), "our mcp command missing:\n{s}");
    // remote mcp: both arms land in gemini's exact accepted shape.
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-http"],
        serde_json::json!({"type": "http", "url": "http://127.0.0.1:39621/mcp", "headers": {}}),
        "http remote arm mismatch:\n{s}"
    );
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-sse"],
        serde_json::json!({"type": "sse", "url": "http://127.0.0.1:39622/sse", "headers": {}}),
        "sse remote arm mismatch:\n{s}"
    );
    // hooks: SessionStart identity + UserPromptSubmit -> BeforeAgent.
    assert!(s.contains("SessionStart"), "SessionStart hook missing:\n{s}");
    assert!(s.contains("BeforeAgent"), "UserPromptSubmit was not mapped to BeforeAgent:\n{s}");
    assert!(s.contains("self-heal"), "SessionStart hook command missing:\n{s}");
    assert!(s.contains("check-restart"), "BeforeAgent hook command missing:\n{s}");
    // the seeded user config survived our merge.
    assert!(s.contains("theirs") && s.contains("their-server"), "seeded mcp server was clobbered:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("dark"), "seeded top-level key was clobbered:\n{s}");

    // commands: one namespaced TOML per command.
    assert!(cmd_file.exists(), "command TOML not written: {}", cmd_file.display());
    let c = fs::read_to_string(&cmd_file).unwrap();
    assert!(c.contains("prompt ="), "command TOML missing prompt:\n{c}");
    assert!(c.contains("description ="), "command TOML missing description:\n{c}");
    assert!(c.contains("Say hello"), "command body not translated to prompt:\n{c}");
    assert!(c.contains("fixture command"), "command description not translated:\n{c}");

    // agents: rendered plugin-prefixed markdown with a namespaced name, no CC model alias.
    assert!(agent_file.exists(), "agent markdown not written: {}", agent_file.display());
    let a = fs::read_to_string(&agent_file).unwrap();
    assert!(a.contains("name: ez-fixture-plugin-ez-helper"), "agent name not namespaced:\n{a}");
    assert!(!a.contains("sonnet"), "the CC model alias must be dropped:\n{a}");
    assert!(a.contains("fixture helper agent"), "agent body not translated:\n{a}");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.gemini.join("settings.json"), cmd_file.clone(), agent_file.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "gemini"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let s = env.settings();
    assert!(!s.contains("ez-fixture"), "our mcp server survived uninstall:\n{s}");
    assert!(!s.contains("self-heal") && !s.contains("check-restart"), "our hooks survived uninstall:\n{s}");
    assert!(s.contains("theirs") && s.contains("their-server"), "uninstall removed the seeded mcp server:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("dark"), "uninstall removed the seeded top-level key:\n{s}");
    assert!(!cmd_dir.exists(), "our command dir survived uninstall: {}", cmd_dir.display());
    assert!(!agent_file.exists(), "our agent file survived uninstall: {}", agent_file.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable settings.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "gemini"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.settings().contains("ez-fixture"), "re-install did not re-add our server");
}

#[test]
fn gemini_uninstall_leaves_no_shell_of_the_containers_it_created() {
    // The class fix, on a backend with no status line: install creates `hooks` from
    // nothing (the seed has none), and a removal that only deletes its own leaf keys
    // leaves `"hooks": {}` behind in a file the user owns. `mcpServers` is the control
    // — it holds a server of theirs, so it must survive with exactly that in it.
    let env = Env::new("no-container-shells");
    let seed: serde_json::Value = serde_json::from_str(SEED_SETTINGS).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "gemini"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    let installed: serde_json::Value = serde_json::from_str(&env.settings()).unwrap();
    assert!(installed.get("hooks").is_some(), "the arm under test needs install to create `hooks`:\n{installed:#}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    let after: serde_json::Value = serde_json::from_str(&env.settings()).unwrap();
    assert_eq!(after, seed, "uninstall must leave the settings file exactly as it found it");
}

#[test]
fn gemini_uninstall_drops_a_settings_file_it_authored() {
    // Nothing but our own writes was ever in this file, so the honest inverse of the
    // install that created it is to take it back out — not to leave a husk of empty
    // containers on a machine where the plugin was never anything but ours.
    let env = Env::new("authored-settings");
    let settings = env.gemini.join("settings.json");
    fs::remove_file(&settings).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "gemini"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert!(settings.exists(), "the arm under test needs install to author the settings file");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert!(!settings.exists(), "uninstall orphaned a settings file it authored:\n{}", fs::read_to_string(&settings).unwrap());
}
