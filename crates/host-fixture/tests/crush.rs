//! Hermetic crush-backend lifecycle, fully isolated from the real
//! `~/.config/crush`. No docker, no auth, no `crush` binary: the backend only ever
//! writes crush's single `crush.json` plus the skills/commands dirs alongside it,
//! so we drive `host_fixture setup --agent crush` against a temp config dir (pinned
//! via `CRUSH_GLOBAL_CONFIG`) and assert the written files by reading them back.
//! `detect()` passes off the pre-created config dir alone (no `crush` on PATH).
//!
//! Every path the backend touches derives from `CRUSH_GLOBAL_CONFIG`/`HOME`, which
//! we point at a throwaway temp root — so proving our entries land there (and the
//! seeded user entries survive) also proves the backend never reaches the real
//! config. The fixture plugin declares no `PreToolUse` hook (its hooks are
//! `SessionStart`/`UserPromptSubmit`, neither of which crush defines), so only mcp
//! translates; the seeded user hook is what verifies the hooks branch of the merge.
//! The fixture's one command (`commands/hello.md`, CC frontmatter + a body) proves
//! the frontmatter-drop: crush's own loader does not strip it, so a written command
//! file must contain the body only, never the source's `---` fence.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server (Typed shape, like crush's own), an unrelated top-level
/// key, and a user `PreToolUse` hook — all of which MUST outlive our install and
/// uninstall untouched. mcp + hooks share this one file, so the hook seed is
/// load-bearing for the same-file never-clobber guarantee.
const SEED_CONFIG: &str = r#"{
  "theme": "dark",
  "mcp": {
    "theirs": { "type": "stdio", "command": "their-server", "args": [], "env": {} }
  },
  "hooks": {
    "PreToolUse": [
      { "command": "their-guard.sh" }
    ]
  }
}
"#;

struct Env {
    root: PathBuf,
    /// The crush config dir, pinned via `CRUSH_GLOBAL_CONFIG`; holds `crush.json`.
    crush: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("crush")` (and every
    /// other backend's PATH probe) stays false and detection rides on the config dir.
    path: OsString,
}

impl Env {
    /// `name` must be unique per test: `std::process::id()` alone is constant across
    /// every test in this binary, so two tests sharing one root would race on it
    /// under cargo's default parallel test threads.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-crush-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let config = root.join("config");
        let env = Env { crush: config.join("crush"), data: root.join("data"), run: root.join("run"), path: fixture_dir(), config, root };
        // Pre-create the crush config dir so detect() passes with no `crush` on PATH,
        // and seed an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.crush).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.crush.join("crush.json"), SEED_CONFIG).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("CRUSH_GLOBAL_CONFIG", &self.crush)
            .env("PATH", &self.path);
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn config_json(&self) -> String {
        fs::read_to_string(self.crush.join("crush.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `crush`, no sibling agent
/// CLIs, so the fan-out stays a pure crush exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn crush_full_lifecycle() {
    let env = Env::new("lifecycle");
    let config_file = env.crush.join("crush.json");
    let skill_dir = env.crush.join("skills").join("ez-skill");
    let skill = skill_dir.join("SKILL.md");
    let cmd_dir = env.crush.join("commands").join("ez-fixture-plugin");
    let cmd_file = cmd_dir.join("hello.md");

    // install: translates our mcp server into crush's single crush.json.
    let (ok, out) = env.fixture(&["setup", "--agent", "crush"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // A fresh, healthy install must self-heal to a true NoOp: probe reads every surface
    // reconcile just wrote and finds no drift. Guards against a probe/reconcile desync
    // (widened surface probe, or probe rendering from the wrong source) that would churn.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    let c = env.config_json();
    // our mcp server landed under the root `mcp` map, Typed shape (explicit stdio type).
    assert!(c.contains("ez-fixture"), "our mcp server key missing:\n{c}");
    assert!(c.contains("host_fixture"), "our mcp command missing:\n{c}");
    assert!(c.contains("\"type\": \"stdio\""), "crush requires an explicit `type: stdio`:\n{c}");
    // the seeded user config survived our merge: server, top-level key, and PreToolUse hook.
    assert!(c.contains("theirs") && c.contains("their-server"), "seeded mcp server was clobbered:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "seeded top-level key was clobbered:\n{c}");
    assert!(c.contains("their-guard.sh"), "seeded user PreToolUse hook was clobbered:\n{c}");

    // remote mcp: both arms land under the root `mcp` map, exact shape.
    let parsed: serde_json::Value = serde_json::from_str(&c).unwrap();
    assert_eq!(
        parsed["mcp"]["ez-fixture-http"],
        serde_json::json!({"type": "http", "url": "http://127.0.0.1:39621/mcp", "headers": {}}),
        "http remote arm mismatch:\n{c}"
    );
    assert_eq!(
        parsed["mcp"]["ez-fixture-sse"],
        serde_json::json!({"type": "sse", "url": "http://127.0.0.1:39622/sse", "headers": {}}),
        "sse remote arm mismatch:\n{c}"
    );

    // skills: bare `<name>/SKILL.md` under ~/.config/crush/skills, name+description ensured, tagged.
    assert!(skill.exists(), "skill SKILL.md not written: {}", skill.display());
    let sk = fs::read_to_string(&skill).unwrap();
    assert!(sk.contains("name: ez-skill") && sk.contains("description:"), "skill frontmatter missing name/description:\n{sk}");
    assert!(sk.contains("x-agentgear") && sk.contains("ez-fixture-plugin"), "ownership tag missing:\n{sk}");
    assert!(skill_dir.join("reference.md").exists(), "skill support file not copied through");

    // commands: body-only markdown under commands/<plugin>/ — crush's loader does not
    // strip frontmatter, so a leaked CC `---` fence would land literally in the prompt.
    assert!(cmd_file.exists(), "command file not written: {}", cmd_file.display());
    let cf = fs::read_to_string(&cmd_file).unwrap();
    assert!(!cf.starts_with("---"), "CC frontmatter fence leaked into the command file:\n{cf}");
    assert!(!cf.contains("description:"), "CC frontmatter key leaked into the command file:\n{cf}");
    assert!(cf.contains("Say hello"), "command body missing:\n{cf}");

    // safety: everything we wrote is under the throwaway temp root.
    assert!(config_file.starts_with(&env.root), "backend wrote outside the temp root: {}", config_file.display());
    assert!(skill.starts_with(&env.root), "skill written outside the temp root");
    assert!(cmd_file.starts_with(&env.root), "command file written outside the temp root");

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "crush"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entry gone, the user's mcp server + key + hook kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let c = env.config_json();
    assert!(!c.contains("ez-fixture"), "our mcp server survived uninstall:\n{c}");
    assert!(c.contains("theirs") && c.contains("their-server"), "uninstall removed the seeded mcp server:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "uninstall removed the seeded top-level key:\n{c}");
    assert!(c.contains("their-guard.sh"), "uninstall removed the seeded user PreToolUse hook:\n{c}");
    assert!(!skill_dir.exists(), "our skill dir survived uninstall: {}", skill_dir.display());
    assert!(!cmd_dir.exists(), "our command dir survived uninstall: {}", cmd_dir.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable crush.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "crush"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.config_json().contains("ez-fixture"), "re-install did not re-add our server");
}
