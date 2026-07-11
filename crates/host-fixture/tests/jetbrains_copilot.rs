//! Hermetic jetbrains-copilot lifecycle, isolated from the real
//! `~/.config/github-copilot`. No docker, no auth, no IDE: the backend only writes
//! `<config>/github-copilot/intellij/mcp.json`, so we drive `host_fixture setup
//! --agent jetbrains-copilot` against a temp `HOME` and assert the written JSON by
//! parsing it back. `detect()` rides on the pre-created `github-copilot/intellij`
//! dir alone (there is no `jetbrains-copilot` binary on PATH).
//!
//! Every path the backend touches derives from `HOME` (Unix: `~/.config/...`), which
//! we point at a throwaway temp root — so proving our file lands under that root
//! (and the seeded user entries survive) also proves it never reaches the real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our install
/// and uninstall untouched. Root key is `servers` (not `mcpServers`).
const SEED_MCP: &str = r#"{
  "inputs": [],
  "servers": {
    "theirs": { "type": "stdio", "command": "their-server", "args": [], "env": {} }
  }
}
"#;

struct Env {
    root: PathBuf,
    /// `<root>/.config/github-copilot/intellij` — where the backend writes mcp.json.
    intellij: PathBuf,
    /// PATH holding only the fixture binary's dir, so no sibling agent CLI detects
    /// and the fan-out stays a pure jetbrains-copilot exercise.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-jbcopilot-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env { intellij: root.join(".config").join("github-copilot").join("intellij"), path: fixture_dir(), root };
        // Pre-create the config dir so detect() passes with no CLI on PATH, and seed an
        // unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.intellij).unwrap();
        fs::write(env.intellij.join("mcp.json"), SEED_MCP).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        // Point every XDG dir at the temp root too, so any other backend's detection
        // (during the unfiltered `uninstall`) stays inside the sandbox.
        cmd.env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
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
        fs::read_to_string(self.intellij.join("mcp.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no sibling agent CLIs, so the
/// fan-out stays a pure jetbrains-copilot exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn jetbrains_copilot_full_lifecycle() {
    let env = Env::new("lifecycle");

    // install: translates our stdio mcp server into mcp.json under `servers`.
    let (ok, out) = env.fixture(&["setup", "--agent", "jetbrains-copilot"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let m = env.mcp();
    // our server landed under `servers` (Typed shape carries `type: stdio`).
    assert!(m.contains("ez-fixture"), "our mcp server key missing:\n{m}");
    assert!(m.contains("host_fixture"), "our mcp command missing:\n{m}");
    assert!(m.contains("\"stdio\""), "typed stdio shape missing:\n{m}");
    // the seeded user config survived our merge.
    assert!(m.contains("theirs") && m.contains("their-server"), "seeded mcp server was clobbered:\n{m}");
    assert!(m.contains("\"inputs\""), "seeded top-level key was clobbered:\n{m}");

    // safety: everything we wrote is under the throwaway temp root.
    assert!(env.intellij.join("mcp.json").starts_with(&env.root), "backend wrote outside the temp root");

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "jetbrains-copilot"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our server gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let m = env.mcp();
    assert!(!m.contains("ez-fixture"), "our mcp server survived uninstall:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "uninstall removed the seeded mcp server:\n{m}");
    assert!(m.contains("\"inputs\""), "uninstall removed the seeded top-level key:\n{m}");

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable mcp.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "jetbrains-copilot"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp().contains("ez-fixture"), "re-install did not re-add our server");
}
