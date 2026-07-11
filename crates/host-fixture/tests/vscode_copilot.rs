//! Hermetic vscode-copilot lifecycle, fully isolated from the real `~/.vscode`.
//!
//! vscode-copilot is **project-scope-only** (`capabilities().scopes == ["project"]`),
//! but the fixture binary only drives `Scope::User` (`setup`/`uninstall` both call
//! into user scope). So the observable end-to-end behavior through the CLI is the
//! scope skip: the backend is *detected* (a pre-created `~/.vscode` dir), yet the
//! fan-out writes nothing at user scope because that scope isn't in its capabilities.
//! This proves the "never write outside a supported scope" invariant against the real
//! binary; the actual `.vscode`/`.github` translation (mcp `servers` key, owned hooks
//! file, `.agent.md` rendering, removal, idempotency, portability filter) is covered
//! exhaustively by the crate's unit tests, which can construct a `Scope::Project`.
//!
//! Every path the backend could touch derives from `HOME`, pointed at a throwaway temp
//! root — so proving nothing lands under that root also proves the developer's real
//! home is never touched.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

struct Env {
    root: PathBuf,
    vscode: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so no `code`/`code-insiders` (or
    /// any sibling agent CLI) resolves and detection rides on `~/.vscode` alone.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-vscodecopilot-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            vscode: root.join(".vscode"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create ~/.vscode so detect() passes with no `code` on PATH; the scope
        // skip (not an absent tool) is then what keeps the backend from writing.
        for dir in [&env.vscode, &env.config, &env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
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
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `code`, no sibling agent CLIs,
/// so the fan-out stays a pure vscode-copilot exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn vscode_copilot_is_detected_but_skipped_at_user_scope() {
    let env = Env::new("scope-skip");

    // setup: the backend is detected (~/.vscode exists) but has no user-scope
    // surface, so the fan-out skips it and the merged outcome is a NoOp.
    let (ok, out) = env.fixture(&["setup", "--agent", "vscode-copilot"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "NoOp", "a project-only backend must not install at user scope, got {out}");

    // nothing was written: no workspace mcp.json under ~/.vscode, no repo .github tree.
    assert!(!env.vscode.join("mcp.json").exists(), "backend wrote a user-scope mcp.json");
    assert!(!env.root.join(".github").exists(), "backend wrote a .github tree at user scope");

    // uninstall is likewise a NoOp (nothing of ours to remove at this scope).
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(out, "NoOp", "uninstall of a never-installed backend must be a NoOp, got {out}");

    // idempotent: a second setup is still a NoOp.
    let (ok, out) = env.fixture(&["setup", "--agent", "vscode-copilot"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");
    assert!(!env.vscode.join("mcp.json").exists(), "second setup wrote a user-scope mcp.json");
}
