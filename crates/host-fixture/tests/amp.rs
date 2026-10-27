//! Hermetic amp-backend lifecycle, fully isolated from the real `~/.config/amp`.
//! No docker, no auth, no `amp` binary: the backend only ever writes amp's
//! `settings.json`, so we drive `host_fixture setup --agent amp` against a temp
//! `XDG_CONFIG_HOME` (+ HOME/XDG data dirs) and assert the written config by
//! reading it back. `detect()` passes off the pre-created config dir alone (no
//! `amp` on PATH).
//!
//! `XDG_CONFIG_HOME` is also set to a *different* dir than `<home>/.config`, so
//! the test proves amp's real precedence (XDG wins over `$HOME/.config`, matching
//! the tool on every OS) rather than merely tolerating an unused env var.
//!
//! Every path the backend touches derives from the redirected env, pointed at a
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
    /// `$XDG_CONFIG_HOME/amp` — amp's user-scope config dir. `XDG_CONFIG_HOME`
    /// itself lives outside `<home>/.config` so a resolver that fell back to
    /// `HOME` instead of honoring the env var would write to the wrong place and
    /// fail the assertions below.
    amp: PathBuf,
    config: PathBuf,
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
        let config = root.join("config");
        let env = Env { amp: config.join("amp"), config, data: root.join("data"), run: root.join("run"), path: fixture_dir(), root };
        // Pre-create $XDG_CONFIG_HOME/amp so detect() passes with no `amp` on PATH,
        // and seed an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.amp).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.amp.join("settings.json"), SEED_CONFIG).unwrap();
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

// --- instructions ------------------------------------------------------------

const INSTR_ID: &str = "ez-fixture-plugin@ez-fixture-plugin";
const INSTR_GUIDANCE: &str = "ez-fixture always-loaded guidance line.\n";

fn instr_block() -> String {
    format!("<!-- agentgear:{INSTR_ID} begin -->\n{INSTR_GUIDANCE}<!-- agentgear:{INSTR_ID} end -->\n")
}

/// The user-scope instructions lifecycle in amp's `$XDG_CONFIG_HOME/amp/AGENTS.md`
/// (one of the guidance files amp always includes from its user config dir): install
/// appends our block after the user's content with one blank separator line; a drifted
/// or deleted block is repaired by self-heal; uninstall strips exactly our block.
#[test]
fn amp_instructions_block_lifecycle() {
    let env = Env::new("instr-lifecycle");
    let agents = env.amp.join("AGENTS.md");
    let seed = "user rule one\nuser rule two\n";
    fs::write(&agents, seed).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "amp"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");
    let installed = format!("{seed}\n{}", instr_block());
    assert_eq!(fs::read_to_string(&agents).unwrap(), installed, "install must append exactly our block after a blank line");

    let (ok, out) = env.fixture(&["setup", "--agent", "amp"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), installed, "a no-op setup must not rewrite AGENTS.md");

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
    assert_eq!(fs::read_to_string(&agents).unwrap(), seed, "uninstall must restore the user's file byte-for-byte");
}

/// With no pre-existing `AGENTS.md`, install creates the file holding only our block,
/// and uninstall — our removal being what emptied it — takes it with it.
#[test]
fn amp_instructions_owns_only_its_file() {
    let env = Env::new("instr-owned-file");
    let agents = env.amp.join("AGENTS.md");
    assert!(!agents.exists(), "precondition: no AGENTS.md seeded");

    let (ok, out) = env.fixture(&["setup", "--agent", "amp"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), instr_block(), "a file we created holds exactly our block, no leading blank line");

    // The Ok line pins the `instructions.is_some()` gate, the push and the user-scope path it is handed.
    let (ok, out) = env.fixture(&["doctor"]);
    assert!(ok, "doctor failed over a healthy install: {out}");
    let want = format!("[ ok ] guidance block present: block present in {}", agents.display());
    assert!(out.lines().any(|l| l == want), "the doctor report lacks an ok guidance-block check over a healthy install:\n{out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert!(!agents.exists(), "uninstall must remove the file our removal emptied");
}

/// A rival agentgear host's block (different plugin id in the markers) and the user's
/// own lines must survive our install and uninstall byte-identical: the marker id
/// scopes ownership, so two agentgear plugins coexist in one AGENTS.md.
#[test]
fn amp_never_clobbers_a_rival_instructions_block() {
    let env = Env::new("instr-rival");
    let agents = env.amp.join("AGENTS.md");
    let rival =
        "user line\n<!-- agentgear:rival-plugin@rival-mkt begin -->\nrival guidance\n<!-- agentgear:rival-plugin@rival-mkt end -->\n";
    fs::write(&agents, rival).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "amp"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(
        fs::read_to_string(&agents).unwrap(),
        format!("{rival}\n{}", instr_block()),
        "install must append our block, leaving the rival's untouched"
    );

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), rival, "uninstall must strip only our block, keeping the rival's and the user's");
}

/// amp also always-includes the SHARED home-level `~/.config/AGENTS.md`, which belongs
/// to the user and every other AGENTS.md reader — never ours to write.
#[test]
fn amp_instructions_leave_the_shared_home_agents_md_unwritten() {
    let env = Env::new("instr-shared-agents");

    let (ok, out) = env.fixture(&["setup", "--agent", "amp"]);
    assert!(ok, "setup failed: {out}");
    assert!(env.amp.join("AGENTS.md").exists(), "precondition: our block landed");

    let shared = env.root.join(".config").join("AGENTS.md");
    assert!(!shared.exists(), "the shared home-level AGENTS.md is never ours to write: {}", shared.display());
}
