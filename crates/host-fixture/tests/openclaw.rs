//! Hermetic openclaw-backend lifecycle, fully isolated from the real `~/.openclaw`.
//! No docker, no auth, no `openclaw` binary: the backend only ever writes openclaw's
//! config, so we drive `host_fixture setup --agent openclaw` against a temp `HOME`
//! (+ XDG dirs) and assert the written `openclaw.json` by parsing it back.
//! `detect()` passes off the pre-created `~/.openclaw` dir alone.
//!
//! openclaw is mcp-only (hooks/commands/agents/skills have no config-writable
//! surface), so this exercises the mcp merge:
//! our server lands under `mcp.servers`, the seeded user entries survive, a second
//! reconcile is a NoOp, and uninstall strips only ours. Every path derives from
//! `HOME`, pointed at a throwaway temp root, so proving our writes land under that
//! root also proves the backend never reaches the developer's real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server (openclaw's own `mcp.servers` shape) + an unrelated
/// top-level key that MUST outlive our install and uninstall untouched.
const SEED_CONFIG: &str = r#"{
  "theme": "dark",
  "mcp": {
    "servers": {
      "theirs": { "command": "their-server", "args": [], "env": {} }
    }
  }
}
"#;

struct Env {
    root: PathBuf,
    /// `~/.openclaw` — the user-scope openclaw config dir.
    openclaw: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("openclaw")` (and every
    /// other backend's PATH probe) stays false and detection rides on `~/.openclaw`.
    path: OsString,
    /// openclaw's own path overrides. Unset by default — `apply` clears every one so a
    /// stray value on the dev box cannot relocate another test's paths.
    overrides: Vec<(&'static str, OsString)>,
}

impl Env {
    /// `name` must be unique per test: `std::process::id()` alone is constant across
    /// every test in this binary, so two tests sharing one root race on it under
    /// cargo's default parallel test threads. Mirrors `tests/gemini.rs`.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-openclaw-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            openclaw: root.join(".openclaw"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            overrides: Vec::new(),
            root,
        };
        // Pre-create ~/.openclaw so detect() passes with no `openclaw` on PATH, and
        // seed an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.openclaw).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.openclaw.join("openclaw.json"), SEED_CONFIG).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("PATH", &self.path);
        for var in ["OPENCLAW_CONFIG_PATH", "OPENCLAW_STATE_DIR", "OPENCLAW_HOME", "OPENCLAW_WORKSPACE_DIR", "OPENCLAW_PROFILE"] {
            cmd.env_remove(var);
        }
        for (var, value) in &self.overrides {
            cmd.env(var, value);
        }
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn config_json(&self) -> String {
        fs::read_to_string(self.openclaw.join("openclaw.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `openclaw`, no sibling agent
/// CLIs, so the fan-out stays a pure openclaw exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn openclaw_full_lifecycle() {
    let env = Env::new("lifecycle");
    let skill_dir = env.openclaw.join("skills").join("ez-skill");
    let skill = skill_dir.join("SKILL.md");

    // install: translates our mcp server into openclaw's config.
    let (ok, out) = env.fixture(&["setup", "--agent", "openclaw"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let c = env.config_json();
    // our mcp server landed under `mcp.servers`, Plain shape (`{command,args,env}`).
    assert!(c.contains("ez-fixture"), "our mcp server key missing:\n{c}");
    assert!(c.contains("host_fixture"), "our mcp command missing:\n{c}");
    // the two-segment key path is present, not a flat `mcpServers`.
    let root: serde_json::Value = serde_json::from_str(&c).unwrap();
    assert!(
        root.get("mcp").and_then(|m| m.get("servers")).and_then(|s| s.get("ez-fixture")).is_some(),
        "server not under mcp.servers.<name>:\n{c}"
    );
    // remote mcp: openclaw's canonical `{url,headers,transport}` shape, rendered
    // directly rather than the majority `{type,url,headers}` dialect, so a probe
    // stays Healthy after openclaw's own `doctor --fix`/`mcp set` would otherwise
    // rewrite it out from under us.
    assert_eq!(
        root["mcp"]["servers"]["ez-fixture-http"],
        serde_json::json!({"url": "http://127.0.0.1:39621/mcp", "headers": {}, "transport": "streamable-http"}),
    );
    assert_eq!(
        root["mcp"]["servers"]["ez-fixture-sse"],
        serde_json::json!({"url": "http://127.0.0.1:39622/sse", "headers": {}, "transport": "sse"}),
    );
    // the seeded user config survived our merge.
    assert!(c.contains("theirs") && c.contains("their-server"), "seeded mcp server was clobbered:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "seeded top-level key was clobbered:\n{c}");

    // skills: bare `<name>/SKILL.md` under ~/.openclaw/skills, name+description ensured, tagged.
    assert!(skill.exists(), "skill SKILL.md not written: {}", skill.display());
    let sk = fs::read_to_string(&skill).unwrap();
    assert!(sk.contains("name: ez-skill") && sk.contains("description:"), "skill frontmatter missing name/description:\n{sk}");
    assert!(sk.contains("x-agentgear") && sk.contains("ez-fixture-plugin"), "ownership tag missing:\n{sk}");
    assert!(skill_dir.join("reference.md").exists(), "skill support file not copied through");

    // safety: everything we wrote is under the throwaway temp root.
    assert!(env.openclaw.join("openclaw.json").starts_with(&env.root), "backend wrote outside the temp root");
    assert!(skill.starts_with(&env.root), "skill written outside the temp root");

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "openclaw"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entry gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let c = env.config_json();
    assert!(!c.contains("ez-fixture"), "our mcp server survived uninstall:\n{c}");
    assert!(c.contains("theirs") && c.contains("their-server"), "uninstall removed the seeded mcp server:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "uninstall removed the seeded top-level key:\n{c}");
    assert!(!skill_dir.exists(), "our skill dir survived uninstall: {}", skill_dir.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable openclaw.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "openclaw"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.config_json().contains("ez-fixture"), "re-install did not re-add our server");
}

// --- instructions ------------------------------------------------------------

const INSTR_ID: &str = "ez-fixture-plugin@ez-fixture-plugin";
const INSTR_GUIDANCE: &str = "ez-fixture always-loaded guidance line.\n";

fn instr_block() -> String {
    format!("<!-- agentgear:{INSTR_ID} begin -->\n{INSTR_GUIDANCE}<!-- agentgear:{INSTR_ID} end -->\n")
}

/// The user-scope instructions lifecycle in openclaw's default-agent workspace
/// `AGENTS.md` (the first of the six bootstrap files it injects into every session,
/// html comments intact): install appends our block after the user's content with one
/// blank separator line; a drifted or deleted block is repaired by self-heal; uninstall
/// strips exactly our block and the separator.
#[test]
fn openclaw_instructions_block_lifecycle() {
    let env = Env::new("instr-lifecycle");
    let agents = env.openclaw.join("workspace").join("AGENTS.md");
    let seed = "# their workspace\nuser rule one\n";
    fs::create_dir_all(agents.parent().unwrap()).unwrap();
    fs::write(&agents, seed).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "openclaw"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");
    let installed = format!("{seed}\n{}", instr_block());
    assert_eq!(fs::read_to_string(&agents).unwrap(), installed, "install must append exactly our block after a blank line");

    // The Ok line pins the `instructions.is_some()` gate, the push and the user-scope path it is handed.
    let (ok, out) = env.fixture(&["doctor"]);
    assert!(ok, "doctor failed over a healthy install: {out}");
    let want = format!("[ ok ] guidance block present: block present in {}", agents.display());
    assert!(out.lines().any(|l| l == want), "the doctor report lacks an ok guidance-block check over a healthy install:\n{out}");

    let (ok, out) = env.fixture(&["setup", "--agent", "openclaw"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), installed, "a no-op setup must not rewrite the workspace AGENTS.md");

    let tampered = format!("{seed}\n<!-- agentgear:{INSTR_ID} begin -->\ntampered\n<!-- agentgear:{INSTR_ID} end -->\n");
    fs::write(&agents, &tampered).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the drifted block: {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), installed, "self-heal must restore the exact block bytes");

    fs::write(&agents, seed).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the deleted block: {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), installed, "self-heal must re-add the block");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), seed, "uninstall must restore the user's workspace file byte-for-byte");
}

/// The cross-tool `~/AGENTS.md` is not an openclaw channel (it opens nothing there) and
/// is owned by every other AGENTS.md reader: install must never write it.
#[test]
fn openclaw_instructions_leaves_the_home_agents_md_unwritten() {
    let env = Env::new("instr-home-agents");

    let (ok, out) = env.fixture(&["setup", "--agent", "openclaw"]);
    assert!(ok, "setup failed: {out}");
    assert!(env.openclaw.join("workspace").join("AGENTS.md").exists(), "precondition: our block landed");

    let home_agents = env.root.join("AGENTS.md");
    assert!(!home_agents.exists(), "the cross-tool ~/AGENTS.md is never ours to write: {}", home_agents.display());
}

/// openclaw's workspace chain starts with `OPENCLAW_WORKSPACE_DIR`; an explicit
/// workspace must take the block, and the default `~/.openclaw/workspace` must then
/// stay untouched.
#[test]
fn openclaw_instructions_follow_the_workspace_override() {
    let mut env = Env::new("instr-workspace-dir");
    let ws = env.root.join("custom-ws");
    fs::create_dir_all(&ws).unwrap();
    env.overrides.push(("OPENCLAW_WORKSPACE_DIR", ws.as_os_str().to_os_string()));

    let (ok, out) = env.fixture(&["setup", "--agent", "openclaw"]);
    assert!(ok, "setup failed: {out}");
    assert!(ws.join("AGENTS.md").exists(), "the block must land in the overridden workspace: {}", ws.display());
    assert!(
        !env.openclaw.join("workspace").join("AGENTS.md").exists(),
        "the default workspace must stay untouched when the override is set"
    );
}

/// `OPENCLAW_CONFIG_PATH` relocates the config FILE only — openclaw's workspace
/// resolver never reads it — so deriving the block from it would write a file openclaw
/// never opens. The block must follow the workspace chain instead.
#[test]
fn openclaw_instructions_ignore_the_config_path_override() {
    let mut env = Env::new("instr-config-path");
    let custom = env.root.join("custom-config");
    fs::create_dir_all(&custom).unwrap();
    env.overrides.push(("OPENCLAW_CONFIG_PATH", custom.join("openclaw.json").into_os_string()));

    let (ok, out) = env.fixture(&["setup", "--agent", "openclaw"]);
    assert!(ok, "setup failed: {out}");
    assert!(
        env.openclaw.join("workspace").join("AGENTS.md").exists(),
        "the block must land in the default workspace, not beside the config file"
    );
    assert!(!custom.join("workspace").join("AGENTS.md").exists(), "the config path's parent is not a workspace root");
}
