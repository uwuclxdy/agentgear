//! Hermetic proof that a plugin tree edited at an UNCHANGED version reaches the box.
//!
//! The class this pins: the materialized dir used to be keyed on the plugin version
//! alone, so the write was skipped for any tree that version had already staged, and
//! `claude` kept serving whatever bytes the version first shipped. It looks like
//! nothing is wrong — the binary is current, the registry is healthy, the version
//! matches — while the hooks and skills the harness loads are however many edits old.
//!
//! Two halves, and the second is what a box actually reads: the staged tree under
//! `versions/` has to carry the edit, and `claude` has to be handed it again. CC copies
//! a plugin tree into its own cache at install time and keys that cache on the plugin
//! version (probed against 2.1.241), so at an unchanged version only an uninstall +
//! install replaces the copy. `fake_claude` models the registry, not the cache copy, so
//! the reinstall is asserted through its call log.
//!
//! No docker, no auth, no real `claude`: the `fake_claude` bin is copied onto a scratch
//! `PATH` as `claude`, and every other backend's detection stays false.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");
const FAKE_CLAUDE: &str = env!("CARGO_BIN_EXE_fake_claude");

/// Mirrors `fake_claude.rs`'s own `CALL_LOG_FILENAME`; a bin target exports nothing an
/// external test crate can import, so this is a second literal by necessity (the same
/// idiom `claude_statusline.rs` uses for the state file's name).
const CALL_LOG_FILENAME: &str = "fake-claude-calls.log";

/// The fixture plugin's own name, which is also its data-root directory.
const PLUGIN_NAME: &str = "ez-fixture-plugin";

/// The marker byte appended to the path source's `commands/hello.md`. The plugin
/// version never moves, so this string is the only thing separating the two trees.
const EDIT_MARKER: &str = "<!-- same-version-edit -->";

struct Env {
    root: PathBuf,
    cfg: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// The mutable `--path` tree this test edits, so the checked-in fixture other
    /// tests read stays untouched.
    src: PathBuf,
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-mat-refresh-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let env = Env {
            cfg: root.join("cfg"),
            data: root.join("data"),
            run: root.join("run"),
            src: root.join("src-plugin"),
            path: fixture_path(&bin),
            root,
        };
        for dir in [&bin, &env.cfg, &env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::copy(FAKE_CLAUDE, bin.join(format!("claude{}", std::env::consts::EXE_SUFFIX))).unwrap();
        copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &env.src);
        env
    }

    fn setup(&self) -> (bool, String) {
        self.fixture(&["setup", "--agent", "claude", "--path", self.src.to_str().unwrap()])
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        self.fixture_env(args, &[])
    }

    /// The same run with extra environment, for the legs that inject a CLI failure.
    fn fixture_env(&self, args: &[&str], extra: &[(&str, &str)]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        for (key, value) in extra {
            cmd.env(key, value);
        }
        // `HOME` + `XDG_CONFIG_HOME` under the temp root keep every non-CC backend's
        // config-dir probe false; the curated `PATH` keeps their `which` probe false.
        cmd.env("CLAUDE_CONFIG_DIR", &self.cfg)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("PATH", &self.path);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// The plugin's own source file this test edits.
    fn source_command(&self) -> PathBuf {
        self.src.join("commands").join("hello.md")
    }

    /// The same file as materialized under the live `current@claude` pointer: what
    /// `claude plugin marketplace add` is pointed at, and what a reinstall copies from.
    fn staged_command(&self) -> String {
        let current = self.data.join(PLUGIN_NAME).join("current@claude");
        fs::read_to_string(current.join("commands").join("hello.md")).unwrap()
    }

    /// This client's staged version dirs, newest name first is irrelevant — the count
    /// is what matters, since a content key with no prune leaks one tree per edit.
    fn staged_version_dirs(&self) -> Vec<String> {
        let versions = self.data.join(PLUGIN_NAME).join("versions");
        let mut names: Vec<String> = fs::read_dir(&versions)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with("@claude"))
            .collect();
        names.sort();
        names
    }

    /// The plugin ids `fake_claude`'s registry currently holds.
    fn registry(&self) -> String {
        fs::read_to_string(self.cfg.join("fake-claude-state.json")).unwrap_or_default()
    }

    /// Delete every stamp marker, leaving the install healthy but unowned — what a
    /// binary that never installed it meets.
    fn clear_markers(&self) {
        let markers = self.data.join(PLUGIN_NAME).join("markers");
        assert!(markers.is_dir(), "no markers dir to clear at {}", markers.display());
        fs::remove_dir_all(&markers).unwrap();
    }

    /// Whether the restart-pending flag stands: what a host's own notice hook reads.
    fn restart_pending(&self) -> bool {
        self.data.join(PLUGIN_NAME).join("restart-pending").exists()
    }

    /// Every `claude` invocation so far, one per line.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.cfg.join(CALL_LOG_FILENAME)).unwrap_or_default().lines().map(str::to_string).collect()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// `PATH` holding the scratch `claude` double and the fixture binary's dir, and nothing
/// else — so every other backend's `which` probe stays false regardless of what the dev
/// box has installed.
fn fixture_path(bin: &Path) -> OsString {
    let fixture_dir = Path::new(BIN).parent().map(Path::to_path_buf).unwrap_or_default();
    std::env::join_paths([bin.to_path_buf(), fixture_dir]).unwrap()
}

fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&entry.path(), &dst_path);
        } else {
            fs::copy(entry.path(), &dst_path).unwrap();
        }
    }
}

fn count_calls(calls: &[String], needle: &str) -> usize {
    calls.iter().filter(|line| line.starts_with(needle)).count()
}

#[test]
fn a_same_version_tree_edit_is_re_staged_and_handed_to_claude_again() {
    let env = Env::new("edit");

    let (ok, out) = env.setup();
    assert!(ok, "first setup failed: {out}");
    assert_eq!(out, "Installed", "first setup did not install");
    assert!(!env.staged_command().contains(EDIT_MARKER), "the marker cannot be in the tree before the edit");
    let installs_after_first = count_calls(&env.calls(), "plugin install");

    // One changed byte, same plugin version: nothing any version comparison can see.
    let original = fs::read_to_string(env.source_command()).unwrap();
    fs::write(env.source_command(), format!("{original}\n{EDIT_MARKER}\n")).unwrap();

    let (ok, out) = env.setup();
    assert!(ok, "second setup failed: {out}");

    // Half one: the staged tree the pointer resolves to carries the edit.
    assert!(
        env.staged_command().contains(EDIT_MARKER),
        "the edit never reached the staged tree; `current@claude` still resolves to the tree this version first shipped:\n{}",
        env.staged_command()
    );

    // Half two: claude was handed it. Its cache copy is version-keyed, so only a
    // reinstall replaces it.
    let calls = env.calls();
    assert!(
        count_calls(&calls, "plugin uninstall") >= 1,
        "a same-version tree change must reinstall, or claude keeps serving its version-keyed cache copy:\n{calls:#?}"
    );
    assert!(count_calls(&calls, "plugin install") > installs_after_first, "the reinstall's install half never ran:\n{calls:#?}");
    assert_eq!(out, "Repaired", "a re-copied tree is a repair, not a no-op");

    // The content key must not leak a full tree copy per edit.
    let staged = env.staged_version_dirs();
    assert_eq!(staged.len(), 1, "the superseded version dir was not pruned: {staged:?}");
}

#[test]
fn self_heal_converges_a_same_version_tree_edit_without_an_explicit_setup() {
    // What "a box is never more than one session behind its plugin tree" rests on. The
    // session-start heal mutates nothing on its `(marker present, Healthy)` row, so the
    // edit has to reach `probe` — no registry read can show it, since the entry, its
    // files and its version all stay correct across a same-version tree change. Without
    // the probe term the two tests above still pass and a real box still never converges.
    let env = Env::new("heal");

    let (ok, out) = env.setup();
    assert!(ok, "first setup failed: {out}");
    let installs_after_first = count_calls(&env.calls(), "plugin install");

    let original = fs::read_to_string(env.source_command()).unwrap();
    fs::write(env.source_command(), format!("{original}\n{EDIT_MARKER}\n")).unwrap();

    // No `--path` here: the heal rehydrates this agent's own source from its marker.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");
    assert_eq!(out, "Repaired", "a heal that re-copied a changed tree must report the repair");
    assert!(env.staged_command().contains(EDIT_MARKER), "self-heal left the staged tree behind the source tree:\n{}", env.staged_command());
    let calls = env.calls();
    assert!(count_calls(&calls, "plugin install") > installs_after_first, "self-heal never handed the new tree to claude:\n{calls:#?}");

    // And the heal is idempotent: the next session converges to a no-op.
    let after_heal = env.calls();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "second self-heal failed: {out}");
    assert_eq!(out, "NoOp", "a converged tree must heal to a no-op");
    assert_eq!(env.calls().iter().filter(|l| l.starts_with("plugin install")).count(), count_calls(&after_heal, "plugin install"));
}

#[test]
fn an_unowned_install_is_re_handed_the_tree_rather_than_adopted() {
    // The marker is the only record of the tree the harness was handed, so wiping it
    // leaves what `claude` holds unaccounted for while entry, files and version all
    // still read correct. Adopting on that read alone stamps ownership over bytes
    // nothing ever compared, and the `(marker present, Healthy)` row then no-ops
    // forever. Embedded on both passes, so the marker is the only variable.
    let env = Env::new("unowned");

    let (ok, out) = env.fixture(&["setup", "--agent", "claude"]);
    assert!(ok, "first setup failed: {out}");
    let baseline = env.calls();
    assert!(!env.restart_pending(), "an install must not raise the restart notice");

    env.clear_markers();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed on an unowned install: {out}");
    assert_eq!(out, "Repaired", "an unowned install must be re-handed its tree, got {out}");
    let calls = env.calls();
    // The uninstall half is the assertion that means anything: CC's cache copy is
    // version-keyed, so an install alone leaves it serving the bytes it already had.
    assert!(
        count_calls(&calls, "plugin uninstall") > count_calls(&baseline, "plugin uninstall")
            && count_calls(&calls, "plugin install") > count_calls(&baseline, "plugin install"),
        "the takeover never re-handed the tree to claude:\n{calls:#?}"
    );
    // The tree moved under the session that is running right now, and a takeover
    // strands it exactly like a repair of our own install does.
    assert!(env.restart_pending(), "the takeover re-handed the tree and left the session with no restart notice");

    // The takeover records the tree it handed over, so the next session no-ops and
    // retires the notice with it.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "the settling heal failed: {out}");
    assert_eq!(out, "NoOp", "a recorded takeover must converge to a no-op");
    assert!(!env.restart_pending(), "a heal that converged nothing must retire the restart notice");
}

#[test]
fn an_unchanged_tree_reinstalls_nothing_on_the_next_pass() {
    // The other half of the gate: keying on content must not make every session
    // reinstall. Without this, the fix above passes for the wrong reason.
    let env = Env::new("stable");

    let (ok, out) = env.setup();
    assert!(ok, "first setup failed: {out}");
    let baseline = env.calls();

    let (ok, out) = env.setup();
    assert!(ok, "second setup failed: {out}");
    assert_eq!(out, "NoOp", "an unchanged tree at an unchanged version must converge to a no-op");

    let calls = env.calls();
    assert_eq!(
        count_calls(&calls, "plugin uninstall"),
        count_calls(&baseline, "plugin uninstall"),
        "an unchanged tree must not reinstall:\n{calls:#?}"
    );
    assert_eq!(
        count_calls(&calls, "plugin install"),
        count_calls(&baseline, "plugin install"),
        "an unchanged tree must not reinstall:\n{calls:#?}"
    );
}

#[test]
fn a_reinstall_whose_install_half_fails_is_repaired_rather_than_forgotten() {
    // The reinstall is two CLI calls, and everything between them is a window where the
    // plugin is gone while our marker still stands. self_heal reads exactly that shape
    // as "the user uninstalled it" and forgets it for good, so without the in-flight
    // record a failed `plugin install` — or a SessionStart hook killed mid-pair —
    // permanently uninstalls the host's plugin and every later session reports NoOp.
    let env = Env::new("interrupted");

    let (ok, out) = env.setup();
    assert!(ok, "first setup failed: {out}");

    let original = fs::read_to_string(env.source_command()).unwrap();
    fs::write(env.source_command(), format!("{original}\n{EDIT_MARKER}\n")).unwrap();

    // The refresh runs its uninstall, then cannot install.
    let (ok, out) = env.fixture_env(&["self-heal"], &[("FAKE_CLAUDE_FAIL_INSTALL", "1")]);
    assert!(!ok, "the injected install failure must surface, got a success: {out}");
    assert!(!env.registry().contains("ez-fixture-plugin@"), "the uninstall half must have landed for this test to mean anything");

    // The next session must put it back, not read our own half-done work as a choice
    // the user made.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "the heal after the interrupted reinstall failed: {out}");
    assert_ne!(out, "Cleared", "self-heal forgot a plugin that only WE had uninstalled");
    assert!(env.registry().contains("ez-fixture-plugin@"), "the plugin was never reinstalled:\n{}", env.registry());
    assert!(env.staged_command().contains(EDIT_MARKER), "the repair landed the old tree");

    // And it settles: no reinstall on the pass after that.
    let after = env.calls();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "the settling heal failed: {out}");
    assert_eq!(out, "NoOp", "a repaired install must converge to a no-op");
    assert_eq!(count_calls(&env.calls(), "plugin uninstall"), count_calls(&after, "plugin uninstall"));
}

#[test]
fn a_path_source_that_vanished_stays_a_no_op_instead_of_failing_every_session() {
    // Reading the source tree to hash it puts a new failure on the healthy path: a
    // `--path` checkout that moved or was reaped. There is no drift to detect without
    // the tree, which is not the same as drift, and a session-start heal that reds
    // forever on a converged install is worse than the staleness it was added to catch.
    let env = Env::new("gone");

    let (ok, out) = env.setup();
    assert!(ok, "first setup failed: {out}");

    fs::remove_dir_all(&env.src).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal must survive a source tree that went away: {out}");
    assert_eq!(out, "NoOp", "a converged install whose source vanished has nothing to repair, got {out}");
}

#[test]
fn a_re_enable_still_lands_a_same_version_tree_edit() {
    // The re-enable arm used to return before the tree was compared, so `setup` on a
    // disabled install reported `Repaired` for a pass that left the box on its old
    // bytes — a success report that is not one, and the task's own verify step fails
    // in that state.
    let env = Env::new("disabled");

    let (ok, out) = env.setup();
    assert!(ok, "first setup failed: {out}");

    let original = fs::read_to_string(env.source_command()).unwrap();
    fs::write(env.source_command(), format!("{original}\n{EDIT_MARKER}\n")).unwrap();
    let disable = Command::new(Path::new(BIN).parent().unwrap().join("fake_claude"))
        .args(["plugin", "disable", "ez-fixture-plugin@ez-fixture-plugin"])
        .env("CLAUDE_CONFIG_DIR", &env.cfg)
        .env("HOME", &env.root)
        .status()
        .unwrap();
    assert!(disable.success(), "seeding the disabled state failed");

    let (ok, out) = env.setup();
    assert!(ok, "setup on a disabled install failed: {out}");
    assert_eq!(out, "Repaired", "a re-enable is a repair");
    assert!(
        env.staged_command().contains(EDIT_MARKER),
        "setup reported a repair while leaving the box on its old tree:\n{}",
        env.staged_command()
    );
}
