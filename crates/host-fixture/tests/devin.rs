//! Hermetic devin-backend lifecycle, fully isolated from the real `~/.config/devin`.
//! No docker, no auth, no `devin` binary: the backend only ever writes devin's own
//! config tree, so we drive `host_fixture setup --agent devin` against a temp
//! `XDG_CONFIG_HOME` (+ HOME/XDG dirs) and assert the written `config.json` (with its
//! skill and agent markdown) by parsing them back. `detect()` passes off the pre-created
//! `~/.config/devin` dir alone (no `devin` on the sandbox PATH).
//!
//! Every path the backend touches derives from `XDG_CONFIG_HOME`, pointed at a
//! throwaway temp root — so proving our files land under that root (and the seeded
//! user entries survive) also proves the backend never reaches the developer's real
//! `~/.config/devin`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server, an unrelated top-level key, and a user's own SessionStart
/// hook (sharing the same event our hook writes into) — all MUST outlive our
/// install and uninstall untouched.
const SEED_CONFIG: &str = r#"{
  "theme": "dark",
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  },
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "their-startup-hook.sh" } ] }
    ]
  }
}
"#;

struct Env {
    root: PathBuf,
    /// The devin user base: `<XDG_CONFIG_HOME>/devin`.
    base: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("devin")` (and every
    /// other backend's PATH probe) stays false and detection rides on the config dir.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-devin-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let config_home = root.join("config");
        let env = Env {
            base: config_home.join("devin"),
            config: config_home,
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create ~/.config/devin so detect() passes with no `devin` on PATH, and
        // seed an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.base).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.base.join("config.json"), SEED_CONFIG).unwrap();
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

    fn config(&self) -> String {
        fs::read_to_string(self.base.join("config.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `devin`, no sibling agent CLIs,
/// so the fan-out stays a pure devin exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn devin_full_lifecycle() {
    let env = Env::new("lifecycle");
    let skill = env.base.join("skills").join("ez-fixture-plugin-hello").join("SKILL.md");
    let skill_dir = env.base.join("skills").join("ez-fixture-plugin-hello");
    let agent = env.base.join("agents").join("ez-fixture-plugin-ez-helper").join("AGENT.md");
    let agent_dir = env.base.join("agents").join("ez-fixture-plugin-ez-helper");
    // The plugin's own `skills/` IR is a DISTINCT surface: it lands as a bare `<name>/`
    // dir in devin's `.agents/skills` scan root, never colliding with the CC-commands-as-
    // skills dir (`<config_base>/skills/ez-fixture-plugin-hello`) above.
    let plugin_skill_dir = env.root.join(".agents").join("skills").join("ez-skill");
    let plugin_skill = plugin_skill_dir.join("SKILL.md");

    // install: translates mcp + hooks + commands (skills) + agents into devin config.
    let (ok, out) = env.fixture(&["setup", "--agent", "devin"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // A fresh, healthy install must self-heal to a true NoOp: probe reads every surface
    // reconcile just wrote and finds no drift. Guards against a probe/reconcile desync
    // (widened surface probe, or probe rendering from the wrong source) that would churn.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    let c = env.config();
    // our mcp server landed under `mcpServers`, Plain shape.
    assert!(c.contains("ez-fixture"), "our mcp server key missing:\n{c}");
    assert!(c.contains("host_fixture"), "our mcp command missing:\n{c}");
    // hooks: SessionStart + UserPromptSubmit map 1:1, under the config `hooks` key.
    assert!(c.contains("SessionStart"), "SessionStart hook missing:\n{c}");
    assert!(c.contains("UserPromptSubmit"), "UserPromptSubmit hook missing:\n{c}");
    assert!(c.contains("self-heal"), "SessionStart hook command missing:\n{c}");
    assert!(c.contains("check-restart"), "UserPromptSubmit hook command missing:\n{c}");
    // the seeded user config survived our merge.
    assert!(c.contains("theirs") && c.contains("their-server"), "seeded mcp server was clobbered:\n{c}");

    // remote mcp: devin's transport discriminator is `transport` (`type` is never
    // read — a `type:"sse"` entry silently loads as http), so the render must
    // carry devin's own key.
    let parsed: serde_json::Value = serde_json::from_str(&c).unwrap();
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-http"],
        serde_json::json!({"url": "http://127.0.0.1:39621/mcp", "transport": "http"}),
        "http remote arm mismatch:\n{c}"
    );
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-sse"],
        serde_json::json!({"url": "http://127.0.0.1:39622/sse", "transport": "sse"}),
        "sse remote arm mismatch:\n{c}"
    );
    assert!(c.contains("\"theme\"") && c.contains("dark"), "seeded top-level key was clobbered:\n{c}");
    assert!(c.contains("their-startup-hook.sh"), "seeded user SessionStart hook was clobbered:\n{c}");

    // commands -> a namespaced devin skill dir.
    assert!(skill.exists(), "skill SKILL.md not written: {}", skill.display());
    let s = fs::read_to_string(&skill).unwrap();
    assert!(s.contains("name:"), "skill missing name frontmatter:\n{s}");
    assert!(s.contains("description:"), "skill missing description frontmatter:\n{s}");
    assert!(s.contains("Say hello"), "command body not translated into the skill:\n{s}");

    // agents -> a namespaced devin subagent dir.
    assert!(agent.exists(), "agent AGENT.md not written: {}", agent.display());
    let a = fs::read_to_string(&agent).unwrap();
    assert!(a.contains("name:"), "agent missing name frontmatter:\n{a}");
    assert!(a.contains("model:") && a.contains("sonnet"), "agent model frontmatter not translated:\n{a}");
    assert!(a.contains("fixture helper agent"), "agent body not translated:\n{a}");

    // plugin skills -> bare `<name>/SKILL.md` in the `.agents/skills` scan root, tagged,
    // and NOT under the config base where the commands-as-skills live.
    assert!(plugin_skill.exists(), "plugin skill SKILL.md not written: {}", plugin_skill.display());
    assert!(!plugin_skill.starts_with(&env.base), "plugin skill collided with the commands-as-skills root");
    let ps = fs::read_to_string(&plugin_skill).unwrap();
    assert!(ps.contains("name: ez-skill") && ps.contains("description:"), "plugin skill frontmatter missing:\n{ps}");
    assert!(ps.contains("x-agentgear") && ps.contains("ez-fixture-plugin"), "ownership tag missing:\n{ps}");
    assert!(plugin_skill_dir.join("reference.md").exists(), "plugin skill support file not copied through");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.base.join("config.json"), skill.clone(), agent.clone(), plugin_skill.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "devin"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let c = env.config();
    assert!(!c.contains("ez-fixture"), "our mcp server survived uninstall:\n{c}");
    assert!(!c.contains("self-heal") && !c.contains("check-restart"), "our hooks survived uninstall:\n{c}");
    assert!(c.contains("theirs") && c.contains("their-server"), "uninstall removed the seeded mcp server:\n{c}");
    assert!(c.contains("\"theme\"") && c.contains("dark"), "uninstall removed the seeded top-level key:\n{c}");
    assert!(c.contains("their-startup-hook.sh"), "uninstall removed the seeded user SessionStart hook:\n{c}");
    assert!(!skill_dir.exists(), "our skill dir survived uninstall: {}", skill_dir.display());
    assert!(!agent_dir.exists(), "our agent dir survived uninstall: {}", agent_dir.display());
    assert!(!plugin_skill_dir.exists(), "our plugin skill dir survived uninstall: {}", plugin_skill_dir.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable config.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "devin"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.config().contains("ez-fixture"), "re-install did not re-add our server");
}
