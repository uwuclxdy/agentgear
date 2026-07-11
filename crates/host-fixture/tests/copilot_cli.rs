//! Hermetic copilot-cli-backend lifecycle, fully isolated from the real `~/.copilot`.
//! No docker, no auth, no `copilot` binary: the backend only ever writes copilot's
//! config files, so we drive `host_fixture setup --agent copilot-cli` against a
//! throwaway `COPILOT_HOME` and assert the written `mcp-config.json` / owned hooks
//! file / agent file by reading them back. `detect()` passes off the pre-created
//! `COPILOT_HOME` dir alone (no `copilot` on PATH).
//!
//! Every path the backend touches derives from `COPILOT_HOME`, which we point at a
//! temp root — so proving our files land under that root (and the seeded user entries
//! survive) also proves the backend never reaches the developer's real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our install
/// and uninstall untouched.
const SEED_MCP: &str = r#"{
  "editorHint": true,
  "mcpServers": {
    "theirs": { "type": "local", "command": "their-server", "args": [] }
  }
}
"#;

/// A user hook file at a DIFFERENT name than ours (copilot loads every `hooks/*.json`);
/// it must survive because we own only `hooks/<plugin>.json`.
const SEED_USER_HOOKS: &str = r#"{ "version": 1, "hooks": { "sessionStart": [ { "type": "command", "bash": "their-hook" } ] } }
"#;

struct Env {
    root: PathBuf,
    copilot: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("copilot")` (and every
    /// other backend's PATH probe) stays false and detection rides on `COPILOT_HOME`.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-copilot-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            copilot: root.join(".copilot"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create COPILOT_HOME so detect() passes with no `copilot` on PATH, and seed
        // an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(env.copilot.join("hooks")).unwrap();
        fs::create_dir_all(env.copilot.join("agents")).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.copilot.join("mcp-config.json"), SEED_MCP).unwrap();
        fs::write(env.copilot.join("hooks").join("user-own.json"), SEED_USER_HOOKS).unwrap();
        fs::write(env.copilot.join("agents").join("user-own.agent.md"), "---\nname: user-own\n---\nkeep me\n").unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("COPILOT_HOME", &self.copilot)
            .env("HOME", &self.root)
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
        fs::read_to_string(self.copilot.join("mcp-config.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `copilot`, no sibling agent
/// CLIs, so the fan-out stays a pure copilot exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn copilot_cli_full_lifecycle() {
    let env = Env::new("lifecycle");
    let hooks_file = env.copilot.join("hooks").join("ez-fixture-plugin.json");
    let user_hooks = env.copilot.join("hooks").join("user-own.json");
    let agent_file = env.copilot.join("agents").join("ez-fixture-plugin-ez-helper.agent.md");
    let user_agent = env.copilot.join("agents").join("user-own.agent.md");

    // install: translates mcp + hooks + agents into copilot's config.
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // mcp: our server landed under `mcpServers` in copilot's `type:"local"` shape.
    let m = env.mcp();
    assert!(m.contains("ez-fixture"), "our mcp server key missing:\n{m}");
    assert!(m.contains("host_fixture"), "our mcp command missing:\n{m}");
    assert!(m.contains("\"local\""), "stdio must render copilot's `local` type:\n{m}");
    // `tools:"*"` is copilot-specific and only our entry carries it, so it proves the
    // shape without a JSON parse.
    assert!(m.contains("\"tools\""), "copilot `tools` field missing:\n{m}");
    // the seeded user config survived our merge.
    assert!(m.contains("theirs") && m.contains("their-server"), "seeded mcp server was clobbered:\n{m}");
    assert!(m.contains("editorHint"), "seeded top-level key was clobbered:\n{m}");

    // hooks: our own file, camelCase events, shell string under `bash`.
    assert!(hooks_file.exists(), "owned hooks file not written: {}", hooks_file.display());
    let h = fs::read_to_string(&hooks_file).unwrap();
    assert!(h.contains("sessionStart"), "SessionStart not mapped:\n{h}");
    assert!(h.contains("userPromptSubmitted"), "UserPromptSubmit not mapped:\n{h}");
    assert!(h.contains("\"bash\""), "hook shell string not under `bash`:\n{h}");
    assert!(h.contains("host_fixture self-heal") && h.contains("host_fixture check-restart"), "hook commands missing:\n{h}");
    // a user hook file at a different name is untouched (we own only <plugin>.json).
    assert!(user_hooks.exists() && fs::read_to_string(&user_hooks).unwrap().contains("their-hook"), "seeded user hook file was touched");

    // agents: one <plugin>-<name>.agent.md; the user's own agent file survives.
    assert!(agent_file.exists(), "agent file not written: {}", agent_file.display());
    let a = fs::read_to_string(&agent_file).unwrap();
    assert!(a.contains(r#"name: "ez-fixture-plugin-ez-helper""#), "agent name not plugin-prefixed / YAML-quoted:\n{a}");
    assert!(!a.contains("model:"), "model alias leaked into the copilot agent file:\n{a}");
    assert!(a.contains("fixture helper agent"), "agent body not preserved:\n{a}");
    assert!(user_agent.exists(), "seeded user agent file was removed on install");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.copilot.join("mcp-config.json"), hooks_file.clone(), agent_file.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let m = env.mcp();
    assert!(!m.contains("ez-fixture"), "our mcp server survived uninstall:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "uninstall removed the seeded mcp server:\n{m}");
    assert!(m.contains("editorHint"), "uninstall removed the seeded top-level key:\n{m}");
    assert!(!hooks_file.exists(), "our hooks file survived uninstall: {}", hooks_file.display());
    assert!(user_hooks.exists(), "uninstall removed the seeded user hook file");
    assert!(!agent_file.exists(), "our agent file survived uninstall: {}", agent_file.display());
    assert!(user_agent.exists(), "uninstall removed the seeded user agent file");

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable mcp-config.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp().contains("ez-fixture"), "re-install did not re-add our server");
}
