//! End-to-end lifecycle against a real `claude` in a fully isolated environment
//! (temp `CLAUDE_CONFIG_DIR` + `HOME` + `XDG_CONFIG_HOME` + `XDG_DATA_HOME` +
//! `XDG_RUNTIME_DIR`, never the real `~/.claude`). Isolating `HOME` +
//! `XDG_CONFIG_HOME` keeps the non-CC backends' `detect()` false (their config
//! dirs don't exist under the temp home), so the fan-out stays a pure Claude
//! exercise and never touches the dev's real `~/.gemini`, `~/.codex`, … Ignored by
//! default — spawns the real CLI. Run with:
//!
//! ```sh
//! cargo test -p host-fixture -- --ignored
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

const PLUGIN_ID: &str = "ez-fixture-plugin@ez-fixture-plugin";
const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

struct Env {
    root: PathBuf,
    cfg: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// Curated `PATH` holding only `claude` + the fixture, so the non-CC backends'
    /// `which` arm stays false regardless of what the machine has installed.
    path: OsString,
}

impl Env {
    // `name` keeps each test's isolated root (and thus its CLAUDE_CONFIG_DIR +
    // XDG_RUNTIME_DIR lock) distinct, so the ignored tests are safe in parallel.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-e2e-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let env = Env { cfg: root.join("cfg"), data: root.join("data"), run: root.join("run"), path: curated_path(), root };
        for dir in [&env.cfg, &env.data, &env.run] {
            std::fs::create_dir_all(dir).unwrap();
        }
        env
    }

    fn apply(&self, cmd: &mut Command) {
        // Two arms of the non-CC `detect()` must both stay false so the fan-out is
        // a pure Claude exercise: `HOME` + `XDG_CONFIG_HOME` under the temp root kill
        // the config-dir arm (no `~/.gemini`, `~/.config/opencode`, …), and the
        // curated `PATH` kills the `which` arm (a dev box may have `codex`/`opencode`
        // installed). Both also pin any write a filled backend might do to the sandbox.
        cmd.env("CLAUDE_CONFIG_DIR", &self.cfg)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("PATH", &self.path);
    }

    /// Run a fixture subcommand, returning (exit_ok, stdout-trimmed).
    fn fixture(&self, sub: &str) -> (bool, String) {
        self.fixture_args(&[sub])
    }

    /// Run the fixture with arbitrary args (e.g. `["setup", "--path", dir]`).
    fn fixture_args(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Run `doctor`. The curated PATH already carries the fixture binary's dir, so
    /// the host-binary check resolves without extra PATH juggling.
    fn doctor(&self) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.arg("doctor");
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string())
    }

    fn plugin_list(&self) -> String {
        let mut cmd = Command::new("claude");
        cmd.args(["plugin", "list", "--json"]);
        self.apply(&mut cmd);
        String::from_utf8_lossy(&cmd.output().unwrap().stdout).trim().to_string()
    }

    fn marketplace_list(&self) -> String {
        let mut cmd = Command::new("claude");
        cmd.args(["plugin", "marketplace", "list", "--json"]);
        self.apply(&mut cmd);
        String::from_utf8_lossy(&cmd.output().unwrap().stdout).trim().to_string()
    }

    fn manual_uninstall(&self) {
        let mut cmd = Command::new("claude");
        cmd.args(["plugin", "uninstall", PLUGIN_ID, "-y"]);
        self.apply(&mut cmd);
        let _ = cmd.output();
    }

    /// Delete the copied cache tree, leaving the plugin registered but its files
    /// gone — a structurally-broken install.
    fn break_cache(&self) {
        let cache = self.cfg.join("plugins/cache/ez-fixture-plugin");
        std::fs::remove_dir_all(&cache).unwrap();
    }

    /// Delete every stamp marker, leaving the plugin installed and healthy but
    /// unowned — what a fresh binary meets on a machine that already has the plugin.
    fn clear_markers(&self) {
        let markers = self.data.join("ez-fixture-plugin/markers");
        assert!(markers.is_dir(), "no markers dir to clear at {}", markers.display());
        std::fs::remove_dir_all(&markers).unwrap();
    }

    /// Delete agentgear's own materialized tree (`versions/` + the `current`
    /// pointer), leaving the stamp markers untouched. `materialize` is idempotent
    /// per version — an existing `versions/<version>` dir is reused without
    /// re-reading its source — so without this, a re-materialize from any source
    /// would silently serve the already-written bytes back and never prove which
    /// source it actually resolved.
    fn break_materialized_cache(&self) {
        let root = self.data.join("ez-fixture-plugin");
        let _ = std::fs::remove_dir_all(root.join("versions"));
        let _ = std::fs::remove_file(root.join("current"));
    }

    fn manual_disable(&self) {
        let mut cmd = Command::new("claude");
        cmd.args(["plugin", "disable", PLUGIN_ID]);
        self.apply(&mut cmd);
        let _ = cmd.output();
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn claude_available() -> bool {
    Command::new("claude").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// The first `name` found on the inherited `PATH`.
fn locate(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| p.is_file())
}

/// Recursively copy a plugin tree into a scratch dir so a test can mutate its
/// own copy without touching the checked-in fixture (which other tests read).
fn copy_dir_all(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&entry.path(), &dst_path);
        } else {
            std::fs::copy(entry.path(), &dst_path).unwrap();
        }
    }
}

/// A `PATH` holding only the fixture binary's dir and `claude`'s dir, so `which`
/// resolves `claude` + `host_fixture` but not a `codex`/`opencode`/… a dev box may
/// have installed. Claude's plugin ops need nothing else on PATH (local
/// marketplace, no git), so this stays a pure Claude exercise.
fn curated_path() -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = Path::new(BIN).parent() {
        dirs.push(dir.to_path_buf());
    }
    if let Some(claude) = locate("claude")
        && let Some(dir) = claude.parent()
    {
        dirs.push(dir.to_path_buf());
    }
    std::env::join_paths(&dirs).unwrap_or_default()
}

#[test]
#[ignore = "spawns the real `claude` CLI; run with --ignored"]
fn full_lifecycle() {
    if !claude_available() {
        eprintln!("skipping: `claude` not on PATH");
        return;
    }
    let env = Env::new("lifecycle");

    // install: idempotent, reaches a real registered state.
    let (ok, out) = env.fixture("setup");
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed");
    assert!(env.plugin_list().contains(PLUGIN_ID), "plugin not registered after setup");

    // second install is a no-op (already converged).
    let (ok, out) = env.fixture("setup");
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // self_heal on a healthy install: zero mutation.
    let (ok, out) = env.fixture("self-heal");
    assert!(ok && out == "NoOp", "self-heal on healthy should no-op, got {out}");

    // never-resurrect: a deliberate manual uninstall must NOT be reinstalled;
    // self_heal clears the stale marker and stays out.
    env.manual_uninstall();
    assert_eq!(env.plugin_list(), "[]", "manual uninstall did not empty the list");
    let (ok, out) = env.fixture("self-heal");
    assert!(ok && out == "Cleared", "self-heal after manual uninstall should clear, got {out}");
    assert_eq!(env.plugin_list(), "[]", "self-heal resurrected a deliberate uninstall");

    // doctor is healthy once the binary is reachable on PATH.
    env.fixture("setup");
    let (ok, report) = env.doctor();
    assert!(ok, "doctor reported unhealthy:\n{report}");
    assert!(report.contains("hashes match"), "doctor missing the tree-hash check:\n{report}");
    assert!(report.contains("marketplace registered"), "doctor missing the marketplace check:\n{report}");

    // uninstall: plugin gone + refcount-gated marketplace removed.
    let (ok, out) = env.fixture("uninstall");
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(env.plugin_list(), "[]", "plugin still present after uninstall");
    assert_eq!(env.marketplace_list(), "[]", "marketplace not refcount-removed after uninstall");
}

#[test]
#[ignore = "spawns the real `claude` CLI; run with --ignored"]
fn install_via_path_source() {
    if !claude_available() {
        eprintln!("skipping: `claude` not on PATH");
        return;
    }
    let env = Env::new("path");
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin");

    // Source::Path materializes an on-disk tree instead of the baked blob; it must
    // reach the same registered state as the embedded path.
    let (ok, out) = env.fixture_args(&["setup", "--path", plugin_dir.to_str().unwrap()]);
    assert!(ok, "path setup failed: {out}");
    assert_eq!(out, "Installed", "path install did not register");
    assert!(env.plugin_list().contains(PLUGIN_ID), "plugin not registered after path setup");

    let (ok, out) = env.fixture("uninstall");
    assert!(ok && out == "Removed", "uninstall after path install failed: {out}");
    assert_eq!(env.plugin_list(), "[]", "plugin still present after uninstall");
}

#[test]
#[ignore = "spawns the real `claude` CLI; run with --ignored"]
fn self_heal_repairs_a_path_install_from_the_persisted_path() {
    if !claude_available() {
        eprintln!("skipping: `claude` not on PATH");
        return;
    }
    let env = Env::new("path-repair");

    // A mutable copy: the stamp marker must persist THIS dir's path (not the
    // checked-in fixture dir), and a later repair must re-read its live bytes.
    let src_plugin = env.root.join("src-plugin");
    copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &src_plugin);

    let (ok, out) = env.fixture_args(&["setup", "--path", src_plugin.to_str().unwrap()]);
    assert!(ok, "path setup failed: {out}");
    assert_eq!(out, "Installed", "path install did not register");
    assert!(env.plugin_list().contains(PLUGIN_ID), "plugin not registered after path setup");

    // Mutate the path source after install: a marker the baked blob never carries,
    // so a re-materialize can only reproduce it by reading this dir again.
    let hello = src_plugin.join("commands/hello.md");
    let original = std::fs::read_to_string(&hello).unwrap();
    std::fs::write(&hello, format!("{original}\n<!-- path-source-marker -->\n")).unwrap();

    // Force a genuine repair: break CC's own cache (files-missing -> NeedsRepair)
    // and agentgear's own materialized cache (so re-materialize can't just reuse
    // the already-written version dir and skip reading the source).
    env.break_cache();
    env.break_materialized_cache();

    let (ok, out) = env.fixture("self-heal");
    assert!(ok, "self-heal errored on a broken path install: {out}");
    assert_eq!(out, "Repaired", "expected repair of a files-missing path install, got {out}");

    // The re-materialized tree must carry the mutation: proof self_heal resolved
    // the SAME --path dir's current bytes, not the binary's baked blob (which
    // never saw the mutation and would fail this assertion).
    let materialized = env.data.join("ez-fixture-plugin/current/commands/hello.md");
    let content = std::fs::read_to_string(&materialized).unwrap_or_default();
    assert!(content.contains("path-source-marker"), "self-heal did not re-materialize from the persisted --path source:\n{content}");

    // doctor must also resolve the persisted path (not error, not fall back to the
    // baked blob) and see a matching tree.
    let (ok, report) = env.doctor();
    assert!(ok, "doctor reported unhealthy after a path repair:\n{report}");
    assert!(report.contains("hashes match"), "doctor should see the path-sourced tree as matching:\n{report}");

    env.fixture("uninstall");
}

#[test]
#[ignore = "spawns the real `claude` CLI; run with --ignored"]
fn update_repairs_a_path_install_from_the_persisted_path() {
    // Mirrors `self_heal_repairs_a_path_install_from_the_persisted_path` but
    // through `update`, which has its own separate resolve site
    // (`install.rs::reconcile_all`) — self_heal reaching the marker correctly
    // proves nothing about update's.
    if !claude_available() {
        eprintln!("skipping: `claude` not on PATH");
        return;
    }
    let env = Env::new("path-update");

    let src_plugin = env.root.join("src-plugin");
    copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &src_plugin);

    let (ok, out) = env.fixture_args(&["setup", "--path", src_plugin.to_str().unwrap()]);
    assert!(ok, "path setup failed: {out}");
    assert_eq!(out, "Installed", "path install did not register");

    let hello = src_plugin.join("commands/hello.md");
    let original = std::fs::read_to_string(&hello).unwrap();
    std::fs::write(&hello, format!("{original}\n<!-- path-source-marker -->\n")).unwrap();

    env.break_cache();
    env.break_materialized_cache();

    let (ok, out) = env.fixture("update");
    assert!(ok, "update errored on a broken path install: {out}");
    assert_eq!(out, "Repaired", "expected repair of a files-missing path install, got {out}");

    let materialized = env.data.join("ez-fixture-plugin/current/commands/hello.md");
    let content = std::fs::read_to_string(&materialized).unwrap_or_default();
    assert!(content.contains("path-source-marker"), "update did not re-materialize from the persisted --path source:\n{content}");

    env.fixture("uninstall");
}

#[test]
#[ignore = "spawns the real `claude` CLI; run with --ignored"]
fn self_heal_repairs_a_broken_install() {
    if !claude_available() {
        eprintln!("skipping: `claude` not on PATH");
        return;
    }
    let env = Env::new("repair");

    let (ok, _) = env.fixture("setup");
    assert!(ok);
    assert!(env.plugin_list().contains(PLUGIN_ID));

    // Registered, but the cache files are gone: self_heal must repair (not clear,
    // since the marker is present and the entry still exists).
    env.break_cache();
    let (ok, out) = env.fixture("self-heal");
    assert!(ok, "self-heal errored on a broken install: {out}");
    assert_eq!(out, "Repaired", "expected repair of a files-missing install, got {out}");

    // A healthy install again + a healthy doctor.
    let (ok, out) = env.fixture("self-heal");
    assert!(ok && out == "NoOp", "post-repair self-heal should no-op, got {out}");

    env.fixture("uninstall");
}

#[test]
#[ignore = "spawns the real `claude` CLI; run with --ignored"]
fn self_heal_adopts_a_healthy_unowned_install() {
    if !claude_available() {
        eprintln!("skipping: `claude` not on PATH");
        return;
    }
    let env = Env::new("adopt");

    let (ok, _) = env.fixture("setup");
    assert!(ok);
    assert!(env.plugin_list().contains(PLUGIN_ID));

    // Marker absent + install healthy: self_heal takes ownership without mutating
    // the install (reconcile no-ops, so the outcome is `Adopted`, not `Repaired`).
    env.clear_markers();
    let (ok, out) = env.fixture("self-heal");
    assert!(ok, "self-heal errored on an unowned healthy install: {out}");
    assert_eq!(out, "Adopted", "expected adoption of a healthy unowned install, got {out}");
    assert!(env.plugin_list().contains(PLUGIN_ID), "adoption disturbed a healthy install");

    // The marker is back, so the next heal takes the owned-and-healthy fast path.
    let (ok, out) = env.fixture("self-heal");
    assert!(ok && out == "NoOp", "post-adopt self-heal should no-op, got {out}");

    env.fixture("uninstall");
}

#[test]
#[ignore = "spawns the real `claude` CLI; run with --ignored"]
fn self_heal_respects_disable_but_explicit_install_reenables() {
    if !claude_available() {
        eprintln!("skipping: `claude` not on PATH");
        return;
    }
    let env = Env::new("disable");

    env.fixture("setup");
    env.manual_disable();
    assert!(env.plugin_list().contains("\"enabled\": false"), "manual disable did not take");

    // self_heal must NOT re-enable a deliberate disable.
    let (ok, out) = env.fixture("self-heal");
    assert!(ok && out == "NoOp", "self-heal should leave a disabled plugin alone, got {out}");
    assert!(env.plugin_list().contains("\"enabled\": false"), "self-heal re-enabled a deliberate disable");

    // An explicit setup DOES re-enable (install flips enable state).
    let (ok, out) = env.fixture("setup");
    assert!(ok && out == "Repaired", "explicit setup should re-enable, got {out}");
    assert!(env.plugin_list().contains("\"enabled\": true"), "explicit setup did not re-enable");

    env.fixture("uninstall");
}

#[test]
#[ignore = "spawns the real `claude` CLI; run with --ignored"]
fn restart_flag_lifecycle() {
    if !claude_available() {
        eprintln!("skipping: `claude` not on PATH");
        return;
    }
    let env = Env::new("restart");

    // A fresh install never sets the flag (install precedes any session that uses
    // the plugin), so the UserPromptSubmit hook stays silent.
    let (ok, _) = env.fixture("setup");
    assert!(ok);
    let (ok, out) = env.fixture("check-restart");
    assert!(ok && out.is_empty(), "check-restart should be silent after install, got {out:?}");

    // A no-op update at the same version is silent too.
    let (ok, out) = env.fixture("update");
    assert!(ok && out == "NoOp", "same-version update should no-op, got {out}");
    let (ok, out) = env.fixture("check-restart");
    assert!(ok && out.is_empty(), "check-restart should be silent after a no-op update, got {out:?}");

    // An update that actually re-materializes (cache files gone) leaves the running
    // session stale; the flag is set and check-restart prints the notice.
    env.break_cache();
    let (ok, _) = env.fixture("update");
    assert!(ok);
    let (ok, out) = env.fixture("check-restart");
    assert!(ok, "check-restart errored: {out}");
    assert!(out.contains("ez-fixture-plugin"), "notice should name the plugin: {out}");
    assert!(out.contains("/reload-plugins"), "notice should name the remedy: {out}");

    // A healthy self_heal (fast path) clears the flag: the running install is current.
    let (ok, out) = env.fixture("self-heal");
    assert!(ok && out == "NoOp", "post-update self-heal should no-op, got {out}");
    let (ok, out) = env.fixture("check-restart");
    assert!(ok && out.is_empty(), "check-restart should be silent once healthy, got {out:?}");

    env.fixture("uninstall");
}
