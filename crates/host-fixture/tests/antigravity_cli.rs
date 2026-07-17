//! Hermetic antigravity-cli lifecycle, fully isolated from the real `~/.gemini`.
//! No docker, no auth, no `agy` binary: the backend only ever writes Antigravity's
//! config files, so we drive `host_fixture setup --agent antigravity-cli` against a
//! temp `HOME` (+ XDG dirs) and assert the written `config/mcp_config.json` /
//! `config/hooks.json` by parsing them back. `detect()` passes off the
//! pre-created `~/.gemini/antigravity-cli` dir alone (no `agy` on PATH).
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

/// A foreign mcp server + an unrelated top-level key that MUST outlive our install
/// and uninstall untouched. Lives in the SHARED `mcp_config.json` the desktop
/// antigravity backend also writes.
const SEED_MCP: &str = r#"{
  "theme": "dark",
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  }
}
"#;

struct Env {
    root: PathBuf,
    mcp: PathBuf,
    hooks: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("agy")` (and every
    /// other backend's PATH probe) stays false and detection rides on the config dir.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-antigravity-cli-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let gemini = root.join(".gemini");
        let env = Env {
            mcp: gemini.join("config").join("mcp_config.json"),
            // `~/.gemini/config/` is agy's global customization root and holds both
            // files; `~/.gemini/antigravity-cli/` (the detect marker below) is the
            // CLI's own settings dir, scanned for neither.
            hooks: gemini.join("config").join("hooks.json"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create ~/.gemini/antigravity-cli so detect() passes with no `agy` on
        // PATH, and seed an unrelated user mcp config the lifecycle must preserve.
        fs::create_dir_all(gemini.join("antigravity-cli")).unwrap();
        fs::create_dir_all(env.mcp.parent().unwrap()).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
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

    fn mcp(&self) -> String {
        fs::read_to_string(&self.mcp).unwrap()
    }

    fn hooks(&self) -> String {
        fs::read_to_string(&self.hooks).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `agy`, no sibling agent CLIs,
/// so the fan-out stays a pure antigravity-cli exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn antigravity_cli_full_lifecycle() {
    let env = Env::new("lifecycle");

    // install: translates mcp (shared file, Plain shape) + hooks (plugin-keyed tree).
    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let m = env.mcp();
    // our mcp server landed under `mcpServers`, Plain shape.
    assert!(m.contains("ez-fixture"), "our mcp server key missing:\n{m}");
    assert!(m.contains("host_fixture"), "our mcp command missing:\n{m}");
    // the seeded user config survived our merge.
    assert!(m.contains("theirs") && m.contains("their-server"), "seeded mcp server was clobbered:\n{m}");

    // remote mcp: `agy` accepts only stdio (`command`) or SSE (`serverUrl`); the
    // shared `{type,url,headers}` shape is refused and voids the whole file.
    let parsed: serde_json::Value = serde_json::from_str(&m).unwrap();
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-sse"],
        serde_json::json!({"serverUrl": "http://127.0.0.1:39622/sse"}),
        "sse remote arm mismatch:\n{m}"
    );
    assert!(parsed["mcpServers"].get("ez-fixture-http").is_none(), "http remote must be skipped (no agy landing):\n{m}");
    assert!(m.contains("\"theme\"") && m.contains("dark"), "seeded top-level key was clobbered:\n{m}");

    // hooks: our plugin-keyed tree. The fixture ships SessionStart + UserPromptSubmit;
    // `agy` has no session-level hook, so only UserPromptSubmit lands, flat under its
    // `PreInvocation` analog. Every name written here must be one agy really fires.
    let h = env.hooks();
    let parsed: serde_json::Value = serde_json::from_str(&h).unwrap();
    assert_eq!(
        parsed["ez-fixture-plugin"],
        serde_json::json!({ "PreInvocation": [{ "type": "command", "command": "host_fixture check-restart" }] }),
        "hook tree mismatch:\n{h}"
    );
    assert!(!h.contains("SessionStart"), "SessionStart is not an agy event; it must be skipped, not written:\n{h}");
    assert!(!h.contains("BeforeAgent"), "BeforeAgent does not exist in agy; it must never be written:\n{h}");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [&env.mcp, &env.hooks] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let m = env.mcp();
    assert!(!m.contains("ez-fixture"), "our mcp server survived uninstall:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "uninstall removed the seeded mcp server:\n{m}");
    assert!(m.contains("\"theme\"") && m.contains("dark"), "uninstall removed the seeded top-level key:\n{m}");

    let h = env.hooks();
    assert!(!h.contains("ez-fixture-plugin"), "our hook key survived uninstall:\n{h}");
    assert!(!h.contains("check-restart"), "our hook command survived uninstall:\n{h}");

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable mcp_config.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp().contains("ez-fixture"), "re-install did not re-add our server");
}
