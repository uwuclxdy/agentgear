//! Hermetic zed-backend lifecycle, fully isolated from the real `~/.config/zed`.
//! No docker, no auth, no `zed` binary: the backend only ever writes zed's
//! `settings.json`, so we drive `host_fixture setup --agent zed` against a temp
//! `XDG_CONFIG_HOME` (+ HOME/XDG data dirs) and assert the written `context_servers`
//! by parsing it back. `detect()` passes off the pre-created config dir alone.
//!
//! Zed is mcp-only, so the only surface asserted is `context_servers`; there are no
//! hook/command files to check. Every path the backend touches derives from the
//! redirected env, so proving our server lands under the temp root (and the seeded
//! user entries survive) also proves the backend never reaches the real config.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// Minimal transcription of zed's `#[serde(untagged)]` `ContextServerSettingsContent`
/// (`crates/settings_content/src/project.rs`, v1.11.3): variant order and required
/// fields as shipped, optional/defaulted fields elided. Enough to prove our written
/// entries land in the intended variant under untagged matching (unknown keys are
/// ignored — zed has no `deny_unknown_fields` here).
#[derive(serde::Deserialize)]
#[serde(untagged)]
#[allow(dead_code)] // fields exist to drive untagged matching, not to be read
enum ZedContextServer {
    Stdio { command: String },
    Http { url: String },
}

/// A foreign context server + an unrelated top-level key that MUST outlive our
/// install and uninstall untouched.
const SEED_SETTINGS: &str = r#"{
  "theme": "One Dark",
  "context_servers": {
    "theirs": { "command": "their-server", "args": [], "env": {} }
  }
}
"#;

struct Env {
    root: PathBuf,
    zed: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("zed")` (and every other
    /// backend's PATH probe) stays false and detection rides on the config dir.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-zed-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let config = root.join("config");
        let env = Env { zed: config.join("zed"), config, data: root.join("data"), run: root.join("run"), path: fixture_dir(), root };
        // Pre-create the config dir so detect() passes with no `zed` on PATH, and seed
        // an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.zed).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.zed.join("settings.json"), SEED_SETTINGS).unwrap();
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
        fs::read_to_string(self.zed.join("settings.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `zed`, no sibling agent CLIs, so
/// the fan-out stays a pure zed exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn zed_full_lifecycle() {
    let env = Env::new("lifecycle");

    // install: translates our stdio mcp server into zed's `context_servers`.
    let (ok, out) = env.fixture(&["setup", "--agent", "zed"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let s = env.settings();
    assert!(s.contains("context_servers"), "context_servers key missing:\n{s}");
    assert!(s.contains("ez-fixture"), "our mcp server key missing:\n{s}");
    assert!(s.contains("host_fixture"), "our mcp command missing:\n{s}");
    // the seeded user config survived our merge.
    assert!(s.contains("theirs") && s.contains("their-server"), "seeded context server was clobbered:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("One Dark"), "seeded top-level key was clobbered:\n{s}");

    // remote mcp: zed's one remote transport is streamable HTTP, modeled as
    // `{url, headers}` with no `type` key; sse has no faithful landing (zed would
    // dial the URL as streamable HTTP) and must be skipped.
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        parsed["context_servers"]["ez-fixture-http"],
        serde_json::json!({"url": "http://127.0.0.1:39621/mcp", "headers": {}}),
        "http remote arm mismatch:\n{s}"
    );
    assert!(parsed["context_servers"].get("ez-fixture-sse").is_none(), "sse remote must be skipped (zed has no sse transport):\n{s}");
    // serde round-trip against zed's settings type: both written entries must
    // deserialize into the intended variant of the untagged enum.
    for (name, want_stdio) in [("ez-fixture", true), ("ez-fixture-http", false)] {
        let entry = parsed["context_servers"][name].clone();
        match serde_json::from_value::<ZedContextServer>(entry).unwrap() {
            ZedContextServer::Stdio { .. } => assert!(want_stdio, "{name} deserialized as stdio, expected remote"),
            ZedContextServer::Http { .. } => assert!(!want_stdio, "{name} deserialized as remote, expected stdio"),
        }
    }

    // safety: everything we wrote is under the throwaway temp root.
    assert!(env.zed.join("settings.json").starts_with(&env.root), "backend wrote outside the temp root");

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "zed"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entry gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let s = env.settings();
    assert!(!s.contains("ez-fixture"), "our mcp server survived uninstall:\n{s}");
    assert!(s.contains("theirs") && s.contains("their-server"), "uninstall removed the seeded context server:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("One Dark"), "uninstall removed the seeded top-level key:\n{s}");

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable settings.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "zed"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.settings().contains("ez-fixture"), "re-install did not re-add our server");
}
