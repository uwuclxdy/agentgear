//! Hermetic qwen-code-backend lifecycle, fully isolated from the real `~/.qwen`.
//! No docker, no auth, no `qwen` binary: the backend only ever writes qwen-code's
//! config tree, so we drive `host_fixture setup --agent qwen-code` against a temp
//! `QWEN_HOME` (+ HOME/XDG dirs) and assert the written `settings.json` (mcp + hooks)
//! plus the copied command markdown and the rendered agent markdown by parsing them
//! back. `detect()` passes off the pre-created `QWEN_HOME` dir alone (no `qwen` on the
//! sandbox PATH).
//!
//! Every path the backend touches derives from `QWEN_HOME`, pointed at a throwaway
//! temp root — so proving our files land under that root (and the seeded user entries
//! survive) also proves the backend never reaches the developer's real `~/.qwen`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server, an unrelated top-level key, and a user's own SessionStart
/// hook (sharing the same event our hook writes into) — all MUST outlive our install
/// and uninstall untouched.
const SEED_SETTINGS: &str = r#"{
  "theme": "dark",
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  },
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "their-startup-hook.sh" } ] }
    ]
  }
}
"#;

struct Env {
    root: PathBuf,
    /// The qwen config base, redirected via `QWEN_HOME`.
    qwen: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("qwen")` (and every
    /// other backend's PATH probe) stays false and detection rides on `QWEN_HOME`.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-qwen-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            qwen: root.join(".qwen"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create QWEN_HOME so detect() passes with no `qwen` on PATH, and seed an
        // unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.qwen).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.qwen.join("settings.json"), SEED_SETTINGS).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("QWEN_HOME", &self.qwen)
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
        fs::read_to_string(self.qwen.join("settings.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `qwen`, no sibling agent CLIs,
/// so the fan-out stays a pure qwen-code exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn qwen_code_full_lifecycle() {
    let env = Env::new("lifecycle");
    let cmd_dir = env.qwen.join("commands").join("ez-fixture-plugin");
    let cmd_file = cmd_dir.join("hello.md");
    let agent_file = env.qwen.join("agents").join("ez-fixture-plugin-ez-helper.md");

    // install: translates mcp + hooks + commands + agents into qwen's config tree.
    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let s = env.settings();
    // our mcp server landed under `mcpServers`, Plain shape (no `type` field).
    assert!(s.contains("ez-fixture"), "our mcp server key missing:\n{s}");
    assert!(s.contains("host_fixture"), "our mcp command missing:\n{s}");
    // hooks: SessionStart + UserPromptSubmit map 1:1, CC's nested shape, under `hooks`.
    assert!(s.contains("SessionStart"), "SessionStart hook missing:\n{s}");
    assert!(s.contains("UserPromptSubmit"), "UserPromptSubmit hook missing:\n{s}");
    assert!(s.contains("self-heal"), "SessionStart hook command missing:\n{s}");
    assert!(s.contains("check-restart"), "UserPromptSubmit hook command missing:\n{s}");
    // the seeded user config survived our merge.
    assert!(s.contains("theirs") && s.contains("their-server"), "seeded mcp server was clobbered:\n{s}");

    // remote mcp: qwen picks transport purely by key presence (`httpUrl` → http,
    // `url` → sse; `type` is never read), so a `url`-keyed http server would
    // silently load over SSE. The render must use qwen's own keys.
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-http"],
        serde_json::json!({"httpUrl": "http://127.0.0.1:39621/mcp"}),
        "http remote arm mismatch:\n{s}"
    );
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-sse"],
        serde_json::json!({"url": "http://127.0.0.1:39622/sse"}),
        "sse remote arm mismatch:\n{s}"
    );
    assert!(s.contains("\"theme\"") && s.contains("dark"), "seeded top-level key was clobbered:\n{s}");
    assert!(s.contains("their-startup-hook.sh"), "seeded user SessionStart hook was clobbered:\n{s}");

    // commands: markdown copy-through under `commands/<plugin>/`.
    assert!(cmd_file.exists(), "command markdown not written: {}", cmd_file.display());
    let c = fs::read_to_string(&cmd_file).unwrap();
    assert!(c.contains("Say hello"), "command body not copied through:\n{c}");

    // agents: rendered plugin-prefixed markdown with a namespaced name, no CC model alias.
    assert!(agent_file.exists(), "agent markdown not written: {}", agent_file.display());
    let a = fs::read_to_string(&agent_file).unwrap();
    assert!(a.contains(r#"name: "ez-fixture-plugin-ez-helper""#), "agent name not namespaced/quoted:\n{a}");
    assert!(!a.contains("sonnet"), "the CC model alias must be dropped:\n{a}");
    assert!(a.contains("fixture helper agent"), "agent body not translated:\n{a}");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.qwen.join("settings.json"), cmd_file.clone(), agent_file.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let s = env.settings();
    assert!(!s.contains("ez-fixture"), "our mcp server survived uninstall:\n{s}");
    assert!(!s.contains("self-heal") && !s.contains("check-restart"), "our hooks survived uninstall:\n{s}");
    assert!(s.contains("theirs") && s.contains("their-server"), "uninstall removed the seeded mcp server:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("dark"), "uninstall removed the seeded top-level key:\n{s}");
    assert!(s.contains("their-startup-hook.sh"), "uninstall removed the seeded user SessionStart hook:\n{s}");
    assert!(!cmd_dir.exists(), "our command dir survived uninstall: {}", cmd_dir.display());
    assert!(!agent_file.exists(), "our agent file survived uninstall: {}", agent_file.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable settings.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.settings().contains("ez-fixture"), "re-install did not re-add our server");
}
