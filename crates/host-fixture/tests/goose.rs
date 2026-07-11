//! Hermetic goose-backend lifecycle, fully isolated from the real `~/.config/goose`
//! and `~/.agents`. No docker, no auth, no `goose` binary: the backend only ever
//! writes goose's file config, so we drive `host_fixture setup --agent goose`
//! against a temp `XDG_CONFIG_HOME`/`HOME` and assert the written `config.yaml`
//! (mcp extensions) + plugin-owned `hooks.json` by reading them back. `detect()`
//! passes off the pre-created `<config>/goose` dir alone (no `goose` on PATH).
//!
//! Every path the backend touches derives from `XDG_CONFIG_HOME`/`HOME`, which we
//! point at a throwaway temp root — so proving our files land under that root (and
//! the seeded user entries survive) also proves it never reaches the real config.
//! YAML round-trip validity is proven indirectly: a second `setup` re-parses the
//! written config and reports `NoOp`, and a post-uninstall re-install parses it
//! again and reports `Installed`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign extension (no `enabled` field, so the self-heal test's `enabled: true`
/// -> `false` edit is unambiguous) + an unrelated top-level key, both of which MUST
/// outlive our install and uninstall untouched.
const SEED_CONFIG: &str = "GOOSE_MODEL: gpt-x\nextensions:\n  theirs:\n    type: stdio\n    cmd: their-server\n";

struct Env {
    root: PathBuf,
    /// `<config>/goose` — the user-scope goose config dir, pre-created for detect().
    goose: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("goose")` (and every
    /// other backend's PATH probe) stays false and detection rides on the config dir.
    path: OsString,
}

impl Env {
    /// `name` must be unique per test: `std::process::id()` alone is constant across
    /// every test in this binary, so two tests sharing one root race on it under
    /// cargo's default parallel test threads. Mirrors `tests/codex.rs`/`opencode.rs`.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-goose-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let config = root.join("config");
        let env = Env { goose: config.join("goose"), data: root.join("data"), run: root.join("run"), path: fixture_dir(), config, root };
        // Pre-create <config>/goose so detect() passes with no `goose` on PATH, and
        // seed an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.goose).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.goose.join("config.yaml"), SEED_CONFIG).unwrap();
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

    fn config_yaml(&self) -> String {
        fs::read_to_string(self.goose.join("config.yaml")).unwrap()
    }

    /// `<HOME>/.agents/plugins/ez-fixture-plugin/hooks/hooks.json` — the plugin-owned
    /// hooks file, keyed off `HOME` (Open Plugins spec dir), not XDG config.
    fn hooks_json(&self) -> PathBuf {
        self.root.join(".agents").join("plugins").join("ez-fixture-plugin").join("hooks").join("hooks.json")
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `goose`, no sibling agent CLIs,
/// so the fan-out stays a pure goose exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn goose_full_lifecycle() {
    let env = Env::new("lifecycle");
    let hooks_file = env.hooks_json();
    let plugin_dir = env.root.join(".agents").join("plugins").join("ez-fixture-plugin");

    // install: translates mcp (extensions) + hooks into goose's config.
    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // mcp: our server landed under `extensions` with goose's own field names.
    let c = env.config_yaml();
    assert!(c.contains("ez-fixture"), "our extension key missing:\n{c}");
    assert!(c.contains("cmd: host_fixture"), "goose `cmd` field missing:\n{c}");
    assert!(c.contains("type: stdio"), "goose `type` field missing:\n{c}");
    assert!(c.contains("enabled: true"), "goose `enabled` field missing:\n{c}");
    // the seeded user config survived our merge.
    assert!(c.contains("theirs") && c.contains("their-server"), "seeded extension was clobbered:\n{c}");
    assert!(c.contains("GOOSE_MODEL"), "seeded top-level key was clobbered:\n{c}");

    // hooks: CC event names pass through 1:1 into the plugin-owned hooks.json.
    assert!(hooks_file.exists(), "hooks.json not written: {}", hooks_file.display());
    let h = fs::read_to_string(&hooks_file).unwrap();
    assert!(h.contains("SessionStart") && h.contains("host_fixture self-heal"), "SessionStart hook missing:\n{h}");
    assert!(h.contains("UserPromptSubmit") && h.contains("host_fixture check-restart"), "UserPromptSubmit hook missing:\n{h}");

    // safety: everything we wrote is under the throwaway temp root.
    for target in [env.goose.join("config.yaml"), hooks_file.clone()] {
        assert!(target.starts_with(&env.root), "backend wrote outside the temp root: {}", target.display());
    }

    // idempotent: a second identical reconcile re-parses the config and no-ops.
    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let c = env.config_yaml();
    assert!(!c.contains("ez-fixture"), "our extension survived uninstall:\n{c}");
    assert!(c.contains("theirs") && c.contains("their-server"), "uninstall removed the seeded extension:\n{c}");
    assert!(c.contains("GOOSE_MODEL"), "uninstall removed the seeded top-level key:\n{c}");
    // the whole plugin-owned hooks dir is dropped (we own it entirely).
    assert!(!plugin_dir.exists(), "our plugin hooks dir survived uninstall: {}", plugin_dir.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (yaml_edit would error on an unparseable config.yaml).
    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.config_yaml().contains("ez-fixture"), "re-install did not re-add our extension");
}

/// goose's `extensions.<name>.enabled` is a real per-extension on/off flag a user
/// can flip by hand. self_heal must classify a user disable as `Disabled` and never
/// write it back to `true` (foundation never-re-enable); an explicit `setup` honors
/// user intent and re-enables, mirroring the claude backend's disabled-entry rule.
#[test]
fn goose_self_heal_never_reenables_a_user_disable() {
    let env = Env::new("self-heal-disable");

    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok && out == "Installed", "initial setup failed: {out}");

    // Simulate the user disabling our extension through goose's own `enabled` flag.
    // The seed's foreign extension has no `enabled` field, so this hits only ours.
    let disabled = env.config_yaml().replace("enabled: true", "enabled: false");
    assert!(disabled.contains("enabled: false"), "seed replace missed the field:\n{disabled}");
    fs::write(env.goose.join("config.yaml"), &disabled).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");
    assert_eq!(out, "NoOp", "self-heal must not touch a deliberately-disabled entry, got {out}");
    assert!(env.config_yaml().contains("enabled: false"), "self-heal re-enabled a user-disabled extension");

    // An explicit re-run of setup honors the user's request and re-enables.
    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok, "re-setup failed: {out}");
    assert_ne!(out, "NoOp", "explicit setup should have re-enabled the disabled entry, got {out}");
    assert!(env.config_yaml().contains("enabled: true"), "explicit setup did not re-enable the disabled entry");
}
