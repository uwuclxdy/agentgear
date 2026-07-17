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
    /// `~/.gemini/antigravity-cli/hooks.json` — the retired user-scope hooks path
    /// this backend wrote to until 2026-07-17 (gotcha 1). `agy` never scanned it;
    /// `reconcile` now sweeps a stray file left there by an old binary.
    retired_hooks: PathBuf,
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
            retired_hooks: gemini.join("antigravity-cli").join("hooks.json"),
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

    // A fresh, healthy install must self-heal to a true NoOp: probe reads every surface
    // reconcile just wrote and finds no drift. Guards against a probe/reconcile desync
    // (widened surface probe, or probe rendering from the wrong source) that would churn.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

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

/// Retired-path sweep (gotcha 1): `reconcile` clears a stray hooks.json an old
/// binary left at `~/.gemini/antigravity-cli/hooks.json` — but only the exact
/// `<plugin>` key it owns there, matching `remove_hooks`'s existing semantics.
#[test]
fn reconcile_sweeps_our_own_key_at_the_retired_hooks_path() {
    let env = Env::new("retired-sweep-tagged");
    fs::write(&env.retired_hooks, r#"{"ez-fixture-plugin":{"Stop":[{"type":"command","command":"stale"}]}}"#).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok, "setup failed: {out}");

    let retired: serde_json::Value = serde_json::from_str(&fs::read_to_string(&env.retired_hooks).unwrap()).unwrap();
    assert!(retired.get("ez-fixture-plugin").is_none(), "our key at the retired path survived reconcile:\n{retired}");
}

/// The retired-path sweep never touches a same-named file it does not own: a
/// foreign plugin's key at that exact dead path must survive byte-for-byte.
#[test]
fn reconcile_leaves_a_foreign_key_at_the_retired_hooks_path_untouched() {
    let env = Env::new("retired-sweep-foreign");
    const FOREIGN: &str = r#"{"someone-elses-plugin":{"Stop":[{"type":"command","command":"their-hook"}]}}"#;
    fs::write(&env.retired_hooks, FOREIGN).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok, "setup failed: {out}");

    assert_eq!(fs::read_to_string(&env.retired_hooks).unwrap(), FOREIGN, "a foreign key at the retired path was touched by the sweep");
}

/// §6: `enabled:false` carry-through. self_heal must never re-enable a hook
/// subtree the user disabled by hand, and the composite probe must read it as
/// `Disabled` (a true no-op, never reaching `reconcile`) even while the marker is
/// present; an explicit `setup` still honors the user's request to re-enable.
#[test]
fn self_heal_preserves_a_disabled_hook_subtree_but_explicit_setup_reenables() {
    let env = Env::new("enabled-false-carry-through");

    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // The user disables our plugin's hooks by hand.
    let mut doc: serde_json::Value = serde_json::from_str(&env.hooks()).unwrap();
    doc["ez-fixture-plugin"]["enabled"] = serde_json::json!(false);
    fs::write(&env.hooks, serde_json::to_vec(&doc).unwrap()).unwrap();
    let before = env.hooks();

    // self_heal (marker present, probe -> Disabled) must be a true no-op: it must
    // never even reach `reconcile` for this backend, let alone flip the flag.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");
    assert_eq!(out, "NoOp", "self_heal must not touch a deliberately disabled hook subtree, got {out}");
    assert_eq!(env.hooks(), before, "self_heal rewrote the disabled hook subtree");

    // An explicit setup (install/update) still honors the user's request to
    // re-enable, exactly like every other backend's `enabled:false` invariant.
    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok, "re-setup failed: {out}");
    assert_ne!(out, "NoOp", "explicit setup must re-enable a disabled hook subtree, got {out}");
    let after: serde_json::Value = serde_json::from_str(&env.hooks()).unwrap();
    assert_ne!(after["ez-fixture-plugin"]["enabled"], serde_json::json!(false), "explicit setup did not re-enable:\n{after}");
}
