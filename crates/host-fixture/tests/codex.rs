//! Hermetic codex-backend lifecycle, fully isolated from the real `~/.codex`. No
//! docker, no auth, no `codex` binary: the backend only ever writes codex's file
//! config, so we drive `host_fixture setup --agent codex` against a temp
//! `CODEX_HOME` (+ HOME/XDG dirs) and assert the written `config.toml` / `hooks.json`
//! / prompt / agent files by reading them back. `detect()` passes off the
//! pre-created codex-home dir alone (no `codex` on PATH).
//!
//! Every path the backend touches derives from `CODEX_HOME`/`HOME`, which we point
//! at a throwaway temp root — so proving our files land under that root (and the
//! seeded user entries survive) also proves the backend never reaches the real
//! config. TOML round-trip parse validity is proven indirectly: a second `setup`
//! must read the written config back (via `toml_edit`) and report `NoOp`, and a
//! post-uninstall re-install must parse it again and report `Installed`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key + a comment that MUST outlive
/// our install and uninstall untouched (comment/key-order preservation is the whole
/// point of the bespoke `toml_edit` path over a naive re-serialize).
const SEED_CONFIG: &str = r#"# the user's own codex config
model = "gpt-5.4"

[mcp_servers.theirs]
command = "their-server"
args = ["--flag"]
"#;

/// A foreign hook under an event we also write to (`SessionStart`, collides with
/// ours) plus one under an event we never touch (`Stop`), both of which MUST
/// outlive our install and uninstall. `hooks.json` is a file separate from
/// `config.toml`, so this seed is load-bearing for hook never-clobber.
const SEED_HOOKS: &str = r#"{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "their-session-hook" } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "command": "their-stop-hook" } ] }
    ]
  }
}
"#;

struct Env {
    root: PathBuf,
    /// `<root>/.codex` — the codex home, pointed at via `CODEX_HOME`.
    codex: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("codex")` (and every
    /// other backend's PATH probe) stays false and detection rides on the codex dir.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-codex-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            codex: root.join(".codex"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create the codex home so detect() passes with no `codex` on PATH, and
        // seed an unrelated user config + hooks the lifecycle must preserve.
        fs::create_dir_all(&env.codex).unwrap();
        for dir in [&env.config, &env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.codex.join("config.toml"), SEED_CONFIG).unwrap();
        fs::write(env.codex.join("hooks.json"), SEED_HOOKS).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("CODEX_HOME", &self.codex)
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

    fn config_toml(&self) -> String {
        fs::read_to_string(self.codex.join("config.toml")).unwrap()
    }

    fn hooks_json(&self) -> String {
        fs::read_to_string(self.codex.join("hooks.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `codex`, no sibling agent
/// CLIs, so the fan-out stays a pure codex exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn codex_full_lifecycle() {
    let env = Env::new("lifecycle");
    let prompt_file = env.codex.join("prompts").join("ez-fixture-plugin-hello.md");
    let agent_file = env.codex.join("agents").join("ez-fixture-plugin-ez-helper.toml");
    let hooks_file = env.codex.join("hooks.json");

    // install: translates mcp + hooks + commands + agents into codex's config.
    let (ok, out) = env.fixture(&["setup", "--agent", "codex"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // mcp: our server landed as a `[mcp_servers.ez-fixture]` table.
    let c = env.config_toml();
    assert!(c.contains("[mcp_servers.ez-fixture]"), "our mcp table missing:\n{c}");
    assert!(c.contains("command = \"host_fixture\""), "our mcp command missing:\n{c}");
    assert!(c.contains("\"mcp\""), "our mcp args missing:\n{c}");
    // the seeded user config survived our merge: server, top-level key, and comment.
    assert!(c.contains("[mcp_servers.theirs]") && c.contains("their-server"), "seeded mcp server was clobbered:\n{c}");
    assert!(c.contains("model = \"gpt-5.4\""), "seeded top-level key was clobbered:\n{c}");
    assert!(c.contains("the user's own codex config"), "seeded comment was dropped (naive re-serialize?):\n{c}");

    // hooks: CC event names pass through 1:1 into hooks.json (inert until /hooks trust).
    assert!(hooks_file.exists(), "hooks.json not written: {}", hooks_file.display());
    let h = env.hooks_json();
    assert!(h.contains("SessionStart"), "SessionStart hook missing:\n{h}");
    assert!(h.contains("UserPromptSubmit"), "UserPromptSubmit hook missing:\n{h}");
    assert!(h.contains("host_fixture self-heal"), "SessionStart hook command missing:\n{h}");
    assert!(h.contains("host_fixture check-restart"), "UserPromptSubmit hook command missing:\n{h}");
    // the seeded user hooks survived: one under a colliding event, one we never touch.
    assert!(h.contains("their-session-hook"), "seeded SessionStart hook was clobbered:\n{h}");
    assert!(h.contains("their-stop-hook"), "seeded Stop hook was clobbered:\n{h}");

    // commands: copy-through markdown prompt, plugin-prefixed + flat.
    assert!(prompt_file.exists(), "prompt markdown not written: {}", prompt_file.display());
    let p = fs::read_to_string(&prompt_file).unwrap();
    assert!(p.contains("Say hello"), "prompt body missing:\n{p}");

    // agents: translated to codex subagent TOML, plugin-prefixed name, model dropped.
    assert!(agent_file.exists(), "agent TOML not written: {}", agent_file.display());
    let a = fs::read_to_string(&agent_file).unwrap();
    assert!(a.contains("name = \"ez-fixture-plugin-ez-helper\""), "agent name not plugin-prefixed:\n{a}");
    assert!(a.contains("developer_instructions"), "agent developer_instructions missing:\n{a}");
    assert!(a.contains("fixture subagent"), "agent description not translated:\n{a}");
    assert!(a.contains("fixture helper agent"), "agent body not translated:\n{a}");
    assert!(!a.contains("model"), "CC model alias leaked into the codex agent (no reliable map):\n{a}");

    // safety: everything we wrote is under the throwaway temp root.
    for target in [env.codex.join("config.toml"), hooks_file.clone(), prompt_file.clone(), agent_file.clone()] {
        assert!(target.starts_with(&env.root), "backend wrote outside the temp root: {}", target.display());
    }

    // idempotent: a second identical reconcile re-parses the config and no-ops.
    let (ok, out) = env.fixture(&["setup", "--agent", "codex"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let c = env.config_toml();
    assert!(!c.contains("ez-fixture"), "our mcp server survived uninstall:\n{c}");
    assert!(c.contains("[mcp_servers.theirs]") && c.contains("their-server"), "uninstall removed the seeded mcp server:\n{c}");
    assert!(c.contains("model = \"gpt-5.4\""), "uninstall removed the seeded top-level key:\n{c}");
    assert!(c.contains("the user's own codex config"), "uninstall dropped the seeded comment:\n{c}");
    let h = env.hooks_json();
    assert!(!h.contains("host_fixture self-heal") && !h.contains("host_fixture check-restart"), "our hooks survived uninstall:\n{h}");
    assert!(h.contains("their-session-hook"), "uninstall removed the seeded SessionStart hook:\n{h}");
    assert!(h.contains("their-stop-hook"), "uninstall removed the seeded Stop hook:\n{h}");
    assert!(!prompt_file.exists(), "prompt file survived uninstall: {}", prompt_file.display());
    assert!(!agent_file.exists(), "agent file survived uninstall: {}", agent_file.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (toml_edit would error on an unparseable config.toml).
    let (ok, out) = env.fixture(&["setup", "--agent", "codex"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.config_toml().contains("[mcp_servers.ez-fixture]"), "re-install did not re-add our server");
}
