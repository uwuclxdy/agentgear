//! Hermetic amp-backend lifecycle, fully isolated from the real `~/.config/amp`.
//! No docker, no auth, no `amp` binary: the backend only ever writes amp's
//! `settings.json`, so we drive `host_fixture setup --agent amp` against a temp
//! `$HOME` (amp resolves `~/.config/amp` through `HOME` on every platform) and
//! assert the written config by reading it back. `detect()` passes off the
//! pre-created `<home>/.config/amp` dir alone (no `amp` on PATH).
//!
//! Every path the backend touches derives from `HOME`/`XDG_*`, pointed at a
//! throwaway temp root — so proving our server lands there (and the seeded user
//! entries survive) also proves it never reaches the real config.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server (under amp's own flat `amp.mcpServers` key) + an unrelated
/// top-level key that MUST outlive our install and uninstall untouched.
const SEED_CONFIG: &str = r#"{
  "theme": "dark",
  "amp.mcpServers": {
    "theirs": { "command": "their-server", "args": [], "env": {} }
  }
}
"#;

struct Env {
    root: PathBuf,
    /// `<home>/.config/amp` — amp's user-scope config dir.
    amp: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("amp")` (and every
    /// other backend's PATH probe) stays false and detection rides on the config dir.
    path: OsString,
}

impl Env {
    /// `name` must be unique per test: `std::process::id()` alone is constant
    /// across every test in this binary, so two tests sharing one root would race
    /// under cargo's default parallel test threads. Mirrors `tests/opencode.rs`.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-amp-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env { amp: root.join(".config").join("amp"), data: root.join("data"), run: root.join("run"), path: fixture_dir(), root };
        // Pre-create <home>/.config/amp so detect() passes with no `amp` on PATH,
        // and seed an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.amp).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.amp.join("settings.json"), SEED_CONFIG).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root).env("XDG_DATA_HOME", &self.data).env("XDG_RUNTIME_DIR", &self.run).env("PATH", &self.path);
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn config_json(&self) -> String {
        fs::read_to_string(self.amp.join("settings.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `amp`, no sibling agent CLIs,
/// so the fan-out stays a pure amp exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn amp_full_lifecycle() {
    let env = Env::new("full-lifecycle");
    let settings = env.amp.join("settings.json");

    // install: translates our mcp server into amp's settings.json.
    let (ok, out) = env.fixture(&["setup", "--agent", "amp"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let c = env.config_json();
    // our mcp server landed under the literal, flat `amp.mcpServers` key.
    assert!(c.contains("\"amp.mcpServers\""), "flat `amp.mcpServers` key missing:\n{c}");
    assert!(c.contains("ez-fixture"), "our mcp server key missing:\n{c}");
    assert!(c.contains("host_fixture"), "our mcp command missing:\n{c}");
    // the seeded user config survived our merge.
    assert!(c.contains("theirs") && c.contains("their-server"), "seeded mcp server was clobbered:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "seeded top-level key was clobbered:\n{c}");

    // remote mcp: both arms land under the flat `amp.mcpServers` key, exact shape.
    let parsed: serde_json::Value = serde_json::from_str(&c).unwrap();
    assert_eq!(
        parsed["amp.mcpServers"]["ez-fixture-http"],
        serde_json::json!({"type": "http", "url": "http://127.0.0.1:39621/mcp", "headers": {}}),
        "http remote arm mismatch:\n{c}"
    );
    assert_eq!(
        parsed["amp.mcpServers"]["ez-fixture-sse"],
        serde_json::json!({"type": "sse", "url": "http://127.0.0.1:39622/sse", "headers": {}}),
        "sse remote arm mismatch:\n{c}"
    );

    // safety: everything we wrote is under the throwaway temp root.
    assert!(settings.starts_with(&env.root), "backend wrote outside the temp root: {}", settings.display());

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "amp"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entry gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let c = env.config_json();
    assert!(!c.contains("ez-fixture"), "our mcp server survived uninstall:\n{c}");
    assert!(c.contains("theirs") && c.contains("their-server"), "uninstall removed the seeded mcp server:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "uninstall removed the seeded top-level key:\n{c}");

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable settings.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "amp"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.config_json().contains("ez-fixture"), "re-install did not re-add our server");
}
