//! Hermetic omp-backend lifecycle, fully isolated from the real `~/.omp`. No docker,
//! no auth, no `omp` binary: the backend only ever writes omp's OMP-native config
//! files, so we drive `host_fixture setup --agent omp` against a temp `HOME` and
//! assert the written `mcp.json` / command / agent files by reading them back.
//! `detect()` passes off the pre-created `~/.omp` dir alone.
//!
//! Every path the backend touches derives from `HOME` (user scope = `~/.omp/agent`),
//! which we point at a throwaway temp root — so proving our files land under that
//! root (and the seeded user entries survive) also proves the backend never reaches
//! the developer's real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key (`$schema`, a real omp key) that
/// MUST outlive our install and uninstall untouched.
const SEED_MCP: &str = r#"{
  "$schema": "https://x/mcp-schema.json",
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  }
}
"#;

struct Env {
    root: PathBuf,
    /// `~/.omp/agent` — the user-scope surface base holding mcp.json/commands/agents.
    base: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("omp")` (and every other
    /// backend's PATH probe) stays false and detection rides on `~/.omp`.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-omp-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            base: root.join(".omp").join("agent"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create ~/.omp/agent so detect() passes with no `omp` on PATH, and seed
        // an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.base).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.base.join("mcp.json"), SEED_MCP).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("PATH", &self.path)
            // A stray PI_CONFIG_DIR/profile on the dev box would relocate the config
            // root; clear them so the backend resolves the default `~/.omp/agent`.
            .env_remove("PI_CONFIG_DIR")
            .env_remove("PI_CODING_AGENT_DIR")
            .env_remove("OMP_PROFILE")
            .env_remove("PI_PROFILE");
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn mcp(&self) -> String {
        fs::read_to_string(self.base.join("mcp.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `omp`, no sibling agent CLIs,
/// so the fan-out stays a pure omp exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn omp_full_lifecycle() {
    let env = Env::new("lifecycle");
    let cmd_file = env.base.join("commands").join("ez-fixture-plugin-hello.md");
    let agent_file = env.base.join("agents").join("ez-fixture-plugin-ez-helper.md");

    // install: translates mcp + commands + agents into omp's config (no hooks surface).
    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let m = env.mcp();
    // our mcp server landed under `mcpServers`, Plain shape (no `type`, omp defaults stdio).
    assert!(m.contains("ez-fixture"), "our mcp server key missing:\n{m}");
    assert!(m.contains("host_fixture"), "our mcp command missing:\n{m}");
    // the seeded user config survived our merge.
    assert!(m.contains("theirs") && m.contains("their-server"), "seeded mcp server was clobbered:\n{m}");
    assert!(m.contains("$schema"), "seeded top-level key was clobbered:\n{m}");

    // remote mcp: both arms land in omp's exact accepted shape.
    let parsed: serde_json::Value = serde_json::from_str(&m).unwrap();
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-http"],
        serde_json::json!({"type": "http", "url": "http://127.0.0.1:39621/mcp", "headers": {}}),
        "http remote arm mismatch:\n{m}"
    );
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-sse"],
        serde_json::json!({"type": "sse", "url": "http://127.0.0.1:39622/sse", "headers": {}}),
        "sse remote arm mismatch:\n{m}"
    );

    // commands: the CC command copies through verbatim, plugin-prefixed + flat.
    assert!(cmd_file.exists(), "command file not written: {}", cmd_file.display());
    let c = fs::read_to_string(&cmd_file).unwrap();
    assert!(c.contains("Say hello"), "command body not copied through:\n{c}");
    assert!(c.contains("description:"), "command frontmatter description missing:\n{c}");

    // agents: re-emitted with a namespaced name, description, and the body as systemPrompt.
    assert!(agent_file.exists(), "agent file not written: {}", agent_file.display());
    let a = fs::read_to_string(&agent_file).unwrap();
    assert!(a.contains("name: ez-fixture-plugin-ez-helper"), "namespaced agent name missing:\n{a}");
    assert!(a.contains("description:"), "agent description missing:\n{a}");
    assert!(!a.contains("sonnet"), "CC model alias leaked into the omp agent:\n{a}");
    assert!(a.contains("fixture helper agent"), "agent body (systemPrompt) missing:\n{a}");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.base.join("mcp.json"), cmd_file.clone(), agent_file.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let m = env.mcp();
    assert!(!m.contains("ez-fixture"), "our mcp server survived uninstall:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "uninstall removed the seeded mcp server:\n{m}");
    assert!(m.contains("$schema"), "uninstall removed the seeded top-level key:\n{m}");
    assert!(!cmd_file.exists(), "our command file survived uninstall: {}", cmd_file.display());
    assert!(!agent_file.exists(), "our agent file survived uninstall: {}", agent_file.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable mcp.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp().contains("ez-fixture"), "re-install did not re-add our server");
}
