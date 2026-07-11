//! Hermetic droid-backend lifecycle, fully isolated from the real `~/.factory`. No
//! docker, no auth, no `droid` binary: the backend only ever writes droid's own config
//! files, so we drive `host_fixture setup --agent droid` against a temp `HOME` (+ XDG
//! dirs) and assert the written `mcp.json` / `hooks.json` / command + droid markdown by
//! parsing them back. `detect()` passes off the pre-created `~/.factory` dir alone (no
//! `droid` on the sandbox PATH).
//!
//! Every path the backend touches derives from `HOME`, pointed at a throwaway temp root
//! — so proving our files land under that root (and the seeded user entries survive)
//! also proves the backend never reaches the developer's real `~/.factory`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our install and
/// uninstall untouched. Lives in droid's dedicated `mcp.json`.
const SEED_MCP: &str = r#"{
  "telemetry": false,
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  }
}
"#;

/// A user's own SessionStart hook (sharing the event our hook writes into) that must
/// survive our merge and our removal. Lives in droid's dedicated `hooks.json`.
const SEED_HOOKS: &str = r#"{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "their-startup-hook.sh" } ] }
    ]
  }
}
"#;

struct Env {
    root: PathBuf,
    factory: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("droid")` (and every other
    /// backend's PATH probe) stays false and detection rides on `~/.factory`.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-droid-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            factory: root.join(".factory"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create ~/.factory so detect() passes with no `droid` on PATH, and seed
        // unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.factory).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.factory.join("mcp.json"), SEED_MCP).unwrap();
        fs::write(env.factory.join("hooks.json"), SEED_HOOKS).unwrap();
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
        fs::read_to_string(self.factory.join("mcp.json")).unwrap()
    }

    fn hooks(&self) -> String {
        fs::read_to_string(self.factory.join("hooks.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `droid`, no sibling agent CLIs, so
/// the fan-out stays a pure droid exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn droid_full_lifecycle() {
    let env = Env::new("lifecycle");
    let command = env.factory.join("commands").join("ez-fixture-plugin-hello.md");
    let droid = env.factory.join("droids").join("ez-fixture-plugin-ez-helper.md");

    // install: translates mcp + hooks + commands + agents into droid's config tree.
    let (ok, out) = env.fixture(&["setup", "--agent", "droid"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // mcp: our server landed under `mcpServers` (Plain shape) in the dedicated mcp.json.
    let m = env.mcp();
    assert!(m.contains("ez-fixture"), "our mcp server key missing:\n{m}");
    assert!(m.contains("host_fixture"), "our mcp command missing:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "seeded mcp server was clobbered:\n{m}");
    assert!(m.contains("telemetry"), "seeded top-level key was clobbered:\n{m}");

    // hooks: SessionStart + UserPromptSubmit map 1:1 into the CC-shape hooks.json wrapper.
    let h = env.hooks();
    assert!(h.contains("SessionStart"), "SessionStart hook missing:\n{h}");
    assert!(h.contains("UserPromptSubmit"), "UserPromptSubmit hook missing:\n{h}");
    assert!(h.contains("self-heal"), "SessionStart hook command missing:\n{h}");
    assert!(h.contains("check-restart"), "UserPromptSubmit hook command missing:\n{h}");
    assert!(h.contains("their-startup-hook.sh"), "seeded user SessionStart hook was clobbered:\n{h}");

    // commands -> a flat, namespaced markdown command (verbatim copy of the CC command).
    assert!(command.exists(), "command markdown not written: {}", command.display());
    let c = fs::read_to_string(&command).unwrap();
    assert!(c.contains("description"), "command frontmatter not carried through:\n{c}");
    assert!(c.contains("Say hello"), "command body not carried through:\n{c}");

    // agents -> a flat, namespaced custom droid with a plugin-prefixed `name`.
    assert!(droid.exists(), "custom droid markdown not written: {}", droid.display());
    let d = fs::read_to_string(&droid).unwrap();
    assert!(d.contains("name: ez-fixture-plugin-ez-helper"), "custom droid name not namespaced:\n{d}");
    assert!(d.contains("model: sonnet"), "custom droid model frontmatter not translated:\n{d}");
    assert!(d.contains("fixture helper agent"), "custom droid body not translated:\n{d}");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.factory.join("mcp.json"), env.factory.join("hooks.json"), command.clone(), droid.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "droid"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let m = env.mcp();
    assert!(!m.contains("ez-fixture"), "our mcp server survived uninstall:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "uninstall removed the seeded mcp server:\n{m}");
    assert!(m.contains("telemetry"), "uninstall removed the seeded top-level key:\n{m}");

    let h = env.hooks();
    assert!(!h.contains("self-heal") && !h.contains("check-restart"), "our hooks survived uninstall:\n{h}");
    assert!(h.contains("their-startup-hook.sh"), "uninstall removed the seeded user SessionStart hook:\n{h}");

    assert!(!command.exists(), "our command file survived uninstall: {}", command.display());
    assert!(!droid.exists(), "our custom droid file survived uninstall: {}", droid.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable mcp.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "droid"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp().contains("ez-fixture"), "re-install did not re-add our server");
}
