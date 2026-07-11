//! Hermetic cursor-backend lifecycle, fully isolated from the real `~/.cursor`.
//! No docker, no auth, no cursor binary: the backend only ever writes cursor's
//! file config, so we drive `host_fixture setup --agent cursor` against a temp
//! `HOME` (+ XDG dirs) and assert the written `mcp.json` / `hooks.json` /
//! `commands` / `agents` by reading them back. `detect()` passes off the
//! pre-created `~/.cursor` dir alone (no `cursor`/`cursor-agent` on PATH).
//!
//! Every path the backend touches derives from `HOME`, which we point at a
//! throwaway temp root — so proving our files land under that root (and the seeded
//! user entries survive) also proves the backend never reaches the real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our
/// install and uninstall untouched.
const SEED_MCP: &str = r#"{
  "editorTelemetry": false,
  "mcpServers": {
    "theirs": { "type": "stdio", "command": "their-server", "args": [] }
  }
}
"#;

/// A foreign hook under an event we also write to (`sessionStart`, collides with
/// our `SessionStart` mapping) plus one under an event we never touch, both of
/// which MUST outlive our install and uninstall. `hooks.json` is a file separate
/// from `mcp.json`, so this seed is load-bearing: without it, hook never-clobber
/// goes completely unverified.
const SEED_HOOKS: &str = r#"{
  "version": 1,
  "hooks": {
    "sessionStart": [
      { "command": "their-session-hook" }
    ],
    "stop": [
      { "command": "their-stop-hook" }
    ]
  }
}
"#;

struct Env {
    root: PathBuf,
    cursor: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("cursor")` /
    /// `which("cursor-agent")` (and every other backend's PATH probe) stays false
    /// and detection rides on `~/.cursor` alone.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-cursor-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            cursor: root.join(".cursor"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create ~/.cursor so detect() passes with no cursor CLI on PATH, and
        // seed an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.cursor).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.cursor.join("mcp.json"), SEED_MCP).unwrap();
        fs::write(env.cursor.join("hooks.json"), SEED_HOOKS).unwrap();
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

    fn mcp(&self) -> String {
        fs::read_to_string(self.cursor.join("mcp.json")).unwrap()
    }

    fn hooks(&self) -> String {
        fs::read_to_string(self.cursor.join("hooks.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `cursor`, no sibling agent
/// CLIs, so the fan-out stays a pure cursor exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn cursor_full_lifecycle() {
    let env = Env::new("lifecycle");
    let cmd_file = env.cursor.join("commands").join("ez-fixture-plugin-hello.md");
    let agent_file = env.cursor.join("agents").join("ez-fixture-plugin-ez-helper.md");
    let hooks_file = env.cursor.join("hooks.json");

    // install: translates mcp + hooks + commands + agents into cursor's config.
    let (ok, out) = env.fixture(&["setup", "--agent", "cursor"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // mcp: our server landed under `mcpServers`, Typed shape (explicit stdio type).
    let m = env.mcp();
    assert!(m.contains("ez-fixture"), "our mcp server key missing:\n{m}");
    assert!(m.contains("host_fixture"), "our mcp command missing:\n{m}");
    assert!(m.contains("\"type\": \"stdio\""), "Typed shape (type:stdio) missing:\n{m}");
    // the seeded user config survived our merge.
    assert!(m.contains("theirs") && m.contains("their-server"), "seeded mcp server was clobbered:\n{m}");
    assert!(m.contains("editorTelemetry"), "seeded top-level key was clobbered:\n{m}");

    // hooks: SessionStart -> sessionStart, UserPromptSubmit -> beforeSubmitPrompt.
    assert!(hooks_file.exists(), "hooks.json not written: {}", hooks_file.display());
    let h = env.hooks();
    assert!(h.contains("\"version\": 1"), "hooks.json missing version:\n{h}");
    assert!(h.contains("sessionStart"), "sessionStart hook missing:\n{h}");
    assert!(h.contains("beforeSubmitPrompt"), "UserPromptSubmit was not mapped to beforeSubmitPrompt:\n{h}");
    assert!(h.contains("self-heal"), "sessionStart hook command missing:\n{h}");
    assert!(h.contains("check-restart"), "beforeSubmitPrompt hook command missing:\n{h}");
    // the seeded user hooks survived our merge: one under an event we also write
    // to (sessionStart), one under an event we never touch.
    assert!(h.contains("their-session-hook"), "seeded sessionStart hook was clobbered:\n{h}");
    assert!(h.contains("their-stop-hook"), "seeded stop hook was clobbered:\n{h}");

    // commands: one plain-markdown file, no frontmatter leaked into the prompt.
    assert!(cmd_file.exists(), "command file not written: {}", cmd_file.display());
    let c = fs::read_to_string(&cmd_file).unwrap();
    assert!(c.contains("Say hello"), "command body not translated:\n{c}");
    assert!(!c.contains("description:"), "frontmatter leaked into the command prompt:\n{c}");

    // agents: one cursor subagent file, plugin-prefixed name, model coerced.
    assert!(agent_file.exists(), "agent file not written: {}", agent_file.display());
    let a = fs::read_to_string(&agent_file).unwrap();
    assert!(a.contains("name: ez-fixture-plugin-ez-helper"), "agent name not plugin-prefixed:\n{a}");
    assert!(a.contains("model: inherit"), "agent model not coerced to inherit:\n{a}");
    assert!(a.contains("fixture helper agent"), "agent body not translated:\n{a}");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.cursor.join("mcp.json"), hooks_file.clone(), cmd_file.clone(), agent_file.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "cursor"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let m = env.mcp();
    assert!(!m.contains("ez-fixture"), "our mcp server survived uninstall:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "uninstall removed the seeded mcp server:\n{m}");
    assert!(m.contains("editorTelemetry"), "uninstall removed the seeded top-level key:\n{m}");
    let h = env.hooks();
    assert!(!h.contains("self-heal") && !h.contains("check-restart"), "our hooks survived uninstall:\n{h}");
    assert!(h.contains("their-session-hook"), "uninstall removed the seeded sessionStart hook:\n{h}");
    assert!(h.contains("their-stop-hook"), "uninstall removed the seeded stop hook:\n{h}");
    assert!(!cmd_file.exists(), "command file survived uninstall: {}", cmd_file.display());
    assert!(!agent_file.exists(), "agent file survived uninstall: {}", agent_file.display());

    // the post-uninstall mcp.json still parses: a clean re-install lands again
    // (json_edit would error on an unparseable mcp.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "cursor"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp().contains("ez-fixture"), "re-install did not re-add our server");
}
