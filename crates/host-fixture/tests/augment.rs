//! Hermetic augment-backend lifecycle, fully isolated from the real `~/.augment`.
//! No docker, no auth, no `auggie` binary: the backend only ever writes augment's
//! config, so we drive `host_fixture setup --agent augment` against a temp `HOME`
//! and assert the written `settings.json` (mcp + hooks in one file) plus the
//! translated command/agent markdown by parsing them back. `detect()` passes off the
//! pre-created `~/.augment` dir alone.
//!
//! Every path the backend touches derives from `HOME`, which we point at a throwaway
//! temp root — so proving our files land under that root (and the seeded user entries
//! survive) also proves the backend never reaches the developer's real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server, an unrelated top-level key, and a user hook under the same
/// event we write to — all must outlive our install and uninstall untouched.
const SEED_SETTINGS: &str = r#"{
  "theme": "dark",
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  },
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "their-session-hook" }] }
    ]
  }
}
"#;

struct Env {
    root: PathBuf,
    augment: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("auggie")` (and every
    /// other backend's PATH probe) stays false and detection rides on `~/.augment`.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-augment-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            augment: root.join(".augment"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create ~/.augment so detect() passes with no `auggie` on PATH, and seed
        // an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.augment).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.augment.join("settings.json"), SEED_SETTINGS).unwrap();
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
        fs::read_to_string(self.augment.join("settings.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `auggie`, no sibling agent CLIs,
/// so the fan-out stays a pure augment exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn augment_full_lifecycle() {
    let env = Env::new("lifecycle");
    let cmd_file = env.augment.join("commands").join("ez-fixture-plugin-hello.md");
    let agent_file = env.augment.join("agents").join("ez-fixture-plugin-ez-helper.md");

    // install: translates mcp + hooks + commands + agents into augment's config.
    let (ok, out) = env.fixture(&["setup", "--agent", "augment"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let s = env.settings();
    // our mcp server landed under `mcpServers`, Plain shape, in the shared settings.json.
    assert!(s.contains("ez-fixture"), "our mcp server key missing:\n{s}");
    assert!(s.contains("host_fixture"), "our mcp command missing:\n{s}");
    // remote mcp: both arms land in augment's exact accepted shape.
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
    // hooks: SessionStart identity maps; UserPromptSubmit has no augment analog -> skipped.
    assert!(s.contains("SessionStart"), "SessionStart hook missing:\n{s}");
    assert!(s.contains("self-heal"), "SessionStart hook command missing:\n{s}");
    assert!(!s.contains("check-restart"), "UserPromptSubmit hook must be skipped (no augment analog):\n{s}");
    assert!(!s.contains("UserPromptSubmit"), "UserPromptSubmit event must not be written:\n{s}");
    // the seeded user config survived our merge (server, top-level key, and same-event hook).
    assert!(s.contains("theirs") && s.contains("their-server"), "seeded mcp server was clobbered:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("dark"), "seeded top-level key was clobbered:\n{s}");
    assert!(s.contains("their-session-hook"), "seeded same-event user hook was clobbered:\n{s}");

    // commands + agents: one plugin-prefixed markdown file each.
    assert!(cmd_file.exists(), "command markdown not written: {}", cmd_file.display());
    let c = fs::read_to_string(&cmd_file).unwrap();
    assert!(c.contains("description:"), "command frontmatter description missing:\n{c}");
    assert!(c.contains("Say hello"), "command body not carried through:\n{c}");
    assert!(agent_file.exists(), "agent markdown not written: {}", agent_file.display());
    let a = fs::read_to_string(&agent_file).unwrap();
    assert!(a.contains("name: ez-fixture-plugin-ez-helper"), "agent name not namespaced:\n{a}");
    assert!(a.contains("fixture helper agent"), "agent body not carried through:\n{a}");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.augment.join("settings.json"), cmd_file.clone(), agent_file.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "augment"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let s = env.settings();
    assert!(!s.contains("ez-fixture"), "our mcp server survived uninstall:\n{s}");
    assert!(!s.contains("self-heal"), "our hook survived uninstall:\n{s}");
    assert!(s.contains("theirs") && s.contains("their-server"), "uninstall removed the seeded mcp server:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("dark"), "uninstall removed the seeded top-level key:\n{s}");
    assert!(s.contains("their-session-hook"), "uninstall removed the seeded user hook:\n{s}");
    assert!(!cmd_file.exists(), "our command file survived uninstall: {}", cmd_file.display());
    assert!(!agent_file.exists(), "our agent file survived uninstall: {}", agent_file.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable settings.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "augment"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.settings().contains("ez-fixture"), "re-install did not re-add our server");
}
