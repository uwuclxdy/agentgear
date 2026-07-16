//! Hermetic antigravity-backend lifecycle, fully isolated from the real `~/.gemini`.
//! No docker, no auth, no desktop app: the backend only ever writes Antigravity
//! 2.0's shared `mcp_config.json`, so we drive `host_fixture setup --agent
//! antigravity` against a temp `HOME` (+ XDG dirs) and assert the written config by
//! parsing it back. `detect()` passes off the pre-created `~/.gemini/antigravity-ide/`
//! IDE marker alone.
//!
//! Every path the backend touches derives from `HOME`, pointed at a throwaway temp
//! root — so proving our MCP entry lands under that root (and the seeded user entries
//! survive) also proves the backend never reaches the developer's real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our install
/// and uninstall untouched. Shape matches what antigravity + antigravity-cli both
/// write (Plain `{command,args,env}` under `mcpServers`, in the shared config file).
const SEED_MCP: &str = r#"{
  "someGlobalSetting": true,
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [], "env": {} }
  }
}
"#;

struct Env {
    root: PathBuf,
    /// The shared `~/.gemini/config/mcp_config.json` the backend writes.
    mcp: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so no sibling agent CLI is on PATH
    /// and detection rides purely on the pre-created IDE marker dir.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-antigravity-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let gemini = root.join(".gemini");
        let env = Env {
            mcp: gemini.join("config").join("mcp_config.json"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create the IDE marker so detect() passes with no CLI on PATH, and seed
        // an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(gemini.join("antigravity-ide")).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::create_dir_all(env.mcp.parent().unwrap()).unwrap();
        fs::write(&env.mcp, SEED_MCP).unwrap();
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

    fn mcp_config(&self) -> String {
        fs::read_to_string(&self.mcp).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `agy`, no sibling agent CLIs, so
/// the fan-out stays a pure antigravity exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn antigravity_full_lifecycle() {
    let env = Env::new("lifecycle");

    // install: translates our mcp server into the shared mcp_config.json.
    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let s = env.mcp_config();
    // our mcp server landed under `mcpServers`, Plain shape.
    assert!(s.contains("ez-fixture"), "our mcp server key missing:\n{s}");
    assert!(s.contains("host_fixture"), "our mcp command missing:\n{s}");
    // the seeded user config survived our merge.
    assert!(s.contains("theirs") && s.contains("their-server"), "seeded mcp server was clobbered:\n{s}");
    assert!(s.contains("someGlobalSetting"), "seeded top-level key was clobbered:\n{s}");

    // remote mcp: antigravity's schema is `additionalProperties:false` — the only
    // remote form is `{serverUrl}` (SSE), and any `type`/`url` key voids the whole
    // file. http has no landing at all and must be skipped, not written.
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-sse"],
        serde_json::json!({"serverUrl": "http://127.0.0.1:39622/sse"}),
        "sse remote arm mismatch:\n{s}"
    );
    assert!(parsed["mcpServers"].get("ez-fixture-http").is_none(), "http remote must be skipped (no antigravity landing):\n{s}");

    // safety: everything we wrote is under the throwaway temp root.
    assert!(env.mcp.starts_with(&env.root), "backend wrote outside the temp root: {}", env.mcp.display());

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entry gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let s = env.mcp_config();
    assert!(!s.contains("ez-fixture"), "our mcp server survived uninstall:\n{s}");
    assert!(s.contains("theirs") && s.contains("their-server"), "uninstall removed the seeded mcp server:\n{s}");
    assert!(s.contains("someGlobalSetting"), "uninstall removed the seeded top-level key:\n{s}");

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable mcp_config.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp_config().contains("ez-fixture"), "re-install did not re-add our server");
}
