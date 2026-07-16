//! Hermetic opencode-backend lifecycle, fully isolated from the real
//! `~/.config/opencode`. No docker, no auth, no `opencode` binary: the backend
//! only ever writes opencode's config, so we drive `host_fixture setup --agent
//! opencode` against a temp `XDG_CONFIG_HOME` (+ HOME/XDG dirs) and assert the
//! written `opencode.json` / translated markdown by reading it back. `detect()`
//! passes off the pre-created `<config>/opencode` dir alone.
//!
//! Every path the backend touches derives from `XDG_CONFIG_HOME`/`HOME`, which we
//! point at a throwaway temp root — so proving our files land under that root (and
//! the seeded user entries survive) also proves it never reaches the real config.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our install
/// and uninstall untouched. The mcp shape mirrors opencode's own (`command` array,
/// `type:"local"`) so the fixture reads like a real user config.
const SEED_CONFIG: &str = r#"{
  "theme": "dark",
  "mcp": {
    "theirs": { "type": "local", "command": ["their-server"], "enabled": true }
  }
}
"#;

struct Env {
    root: PathBuf,
    /// `<config>/opencode` — the user-scope opencode config dir.
    opencode: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("opencode")` (and every
    /// other backend's PATH probe) stays false and detection rides on the config dir.
    path: OsString,
}

impl Env {
    /// `name` must be unique per test: `std::process::id()` alone is constant
    /// across every test in this binary, so two tests sharing one root race on it
    /// under cargo's default parallel test threads (one's `remove_dir_all` can wipe
    /// the other's mid-flight fixture invocation). Mirrors `tests/cline.rs`/`e2e.rs`.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-opencode-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let config = root.join("config");
        let env =
            Env { opencode: config.join("opencode"), data: root.join("data"), run: root.join("run"), path: fixture_dir(), config, root };
        // Pre-create <config>/opencode so detect() passes with no `opencode` on PATH,
        // and seed an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.opencode).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.opencode.join("opencode.json"), SEED_CONFIG).unwrap();
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

    fn config_json(&self) -> String {
        fs::read_to_string(self.opencode.join("opencode.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `opencode`, no sibling agent
/// CLIs, so the fan-out stays a pure opencode exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn opencode_full_lifecycle() {
    let env = Env::new("full-lifecycle");
    let cmd_file = env.opencode.join("commands").join("ez-fixture-plugin-hello.md");
    let agent_file = env.opencode.join("agents").join("ez-fixture-plugin-ez-helper.md");

    // install: translates mcp + commands + agents into opencode's config.
    let (ok, out) = env.fixture(&["setup", "--agent", "opencode"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let c = env.config_json();
    // our mcp server landed under the top-level `mcp` object, bespoke opencode shape.
    assert!(c.contains("ez-fixture"), "our mcp server key missing:\n{c}");
    assert!(c.contains("host_fixture"), "our mcp command missing:\n{c}");
    assert!(c.contains("\"local\""), "opencode `type: local` missing:\n{c}");
    assert!(c.contains("\"enabled\""), "opencode `enabled` flag missing:\n{c}");
    // the seeded user config survived our merge.
    assert!(c.contains("theirs") && c.contains("their-server"), "seeded mcp server was clobbered:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "seeded top-level key was clobbered:\n{c}");

    // remote mcp: both arms land under the root `mcp` object, exact shape.
    let parsed: serde_json::Value = serde_json::from_str(&c).unwrap();
    assert_eq!(
        parsed["mcp"]["ez-fixture-http"],
        serde_json::json!({"type": "remote", "url": "http://127.0.0.1:39621/mcp", "enabled": true}),
        "http remote arm mismatch:\n{c}"
    );
    assert_eq!(
        parsed["mcp"]["ez-fixture-sse"],
        serde_json::json!({"type": "remote", "url": "http://127.0.0.1:39622/sse", "enabled": true}),
        "sse remote arm mismatch:\n{c}"
    );

    // commands: copy-through markdown, plugin-prefixed + flat (discoverable).
    assert!(cmd_file.exists(), "command markdown not written: {}", cmd_file.display());
    let cmd = fs::read_to_string(&cmd_file).unwrap();
    assert!(cmd.contains("Say hello"), "command body missing:\n{cmd}");
    assert!(cmd.contains("description"), "command frontmatter missing:\n{cmd}");

    // agents: translated to opencode subagent markdown with `mode: subagent`.
    assert!(agent_file.exists(), "agent markdown not written: {}", agent_file.display());
    let agent = fs::read_to_string(&agent_file).unwrap();
    assert!(agent.contains("mode: subagent"), "agent `mode: subagent` missing:\n{agent}");
    assert!(agent.contains("fixture subagent"), "agent description not translated:\n{agent}");
    assert!(agent.contains("fixture helper agent"), "agent body not translated:\n{agent}");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.opencode.join("opencode.json"), cmd_file.clone(), agent_file.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "opencode"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let c = env.config_json();
    assert!(!c.contains("ez-fixture"), "our mcp server survived uninstall:\n{c}");
    assert!(c.contains("theirs") && c.contains("their-server"), "uninstall removed the seeded mcp server:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "uninstall removed the seeded top-level key:\n{c}");
    assert!(!cmd_file.exists(), "our command file survived uninstall: {}", cmd_file.display());
    assert!(!agent_file.exists(), "our agent file survived uninstall: {}", agent_file.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable opencode.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "opencode"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.config_json().contains("ez-fixture"), "re-install did not re-add our server");
}

/// opencode's `mcp.<name>.enabled` is a real per-server on/off flag the user can
/// flip by hand (unlike the shared json family, which has no disable notion).
/// self_heal must classify that as `Disabled` and never write it back to `true`
/// (foundation §0 never-re-enable); an explicit `setup` honors user intent and
/// re-enables, mirroring the claude backend's own disabled-entry handling.
#[test]
fn opencode_self_heal_never_reenables_a_user_disable() {
    let env = Env::new("self-heal-disable");

    let (ok, out) = env.fixture(&["setup", "--agent", "opencode"]);
    assert!(ok && out == "Installed", "initial setup failed: {out}");

    // Simulate the user disabling our server through opencode's own `enabled` flag.
    let disabled = env.config_json().replace("\"enabled\": true", "\"enabled\": false");
    assert!(disabled.contains("\"enabled\": false"), "seed replace missed the field:\n{disabled}");
    fs::write(env.opencode.join("opencode.json"), &disabled).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");
    assert_eq!(out, "NoOp", "self-heal must not touch a deliberately-disabled entry, got {out}");
    let c = env.config_json();
    assert!(c.contains("\"enabled\": false"), "self-heal re-enabled a user-disabled mcp server:\n{c}");

    // An explicit re-run of setup (install/update) honors the user's request and
    // re-enables - the same "install flips enable state" rule the claude backend
    // documents for its own disabled-entry handling.
    let (ok, out) = env.fixture(&["setup", "--agent", "opencode"]);
    assert!(ok, "re-setup failed: {out}");
    assert_ne!(out, "NoOp", "explicit setup should have re-enabled the disabled entry, got {out}");
    let c = env.config_json();
    assert!(c.contains("\"enabled\": true"), "explicit setup did not re-enable the disabled entry:\n{c}");
}
