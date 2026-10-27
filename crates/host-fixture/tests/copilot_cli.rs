//! Hermetic copilot-cli tests, fully isolated from a real `~/.copilot`. No
//! docker, no auth, no real `copilot`: the `fake_copilot` bin is staged onto a
//! scratch `PATH` as `copilot`, so the backend's CLI orchestration runs against a
//! throwaway registry while the parts under test are real code writing real files.
//!
//! Two contracts live here: the same-version tree refresh (copilot's install copy
//! is keyed on the plugin, so only a reinstall replaces it), and the retired-slot
//! contract — setup/self-heal/uninstall never touch the user's `settings.json`, so
//! a pre-existing statusLine survives byte-identical.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");
const FAKE_COPILOT: &str = env!("CARGO_BIN_EXE_fake_copilot");

/// The user's own settings: an unrelated key plus a real status line of theirs. The
/// automatic slot wiring is retired, so this whole file must survive every
/// lifecycle op byte-identical — nothing this backend writes lives in it anymore.
const SEED_SETTINGS: &str = r#"{
  "theirSetting": true,
  "statusLine": {
    "type": "command",
    "command": "echo their-bar-row",
    "padding": 2
  }
}
"#;

struct Env {
    root: PathBuf,
    /// `$COPILOT_HOME` — the config dir holding the user's `settings.json` and the
    /// double's own registry state.
    copilot_home: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// `PATH` holding the scratch `copilot` double and the fixture binary's dir, and
    /// nothing else — so every other backend's `which` probe stays false regardless of
    /// what the dev box has installed.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-copilot-statusline-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let env =
            Env { copilot_home: root.join("copilot"), data: root.join("data"), run: root.join("run"), path: curated_path(&bin), root };
        for dir in [&bin, &env.copilot_home, &env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        stage_double(FAKE_COPILOT, &bin.join(format!("copilot{}", std::env::consts::EXE_SUFFIX)));
        env
    }

    fn apply(&self, cmd: &mut Command) {
        // `HOME` + `XDG_CONFIG_HOME` under the temp root keep every other backend's
        // config-dir probe false; the curated `PATH` keeps their `which` probe false.
        // Both also pin any write to the sandbox. `COPILOT_HOME` is what the backend
        // (and the double) resolve their config dir from, ahead of `HOME`.
        cmd.env("COPILOT_HOME", &self.copilot_home)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
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

    fn settings_path(&self) -> PathBuf {
        self.copilot_home.join("settings.json")
    }

    fn seed_settings(&self) {
        fs::write(self.settings_path(), SEED_SETTINGS).unwrap();
    }

    fn settings(&self) -> String {
        fs::read_to_string(self.settings_path()).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn curated_path(scratch_bin: &Path) -> OsString {
    let mut dirs = vec![scratch_bin.to_path_buf()];
    if let Some(dir) = Path::new(BIN).parent() {
        dirs.push(dir.to_path_buf());
    }
    std::env::join_paths(&dirs).unwrap_or_default()
}

/// Stages a CLI double onto the scratch `PATH`: a symlink on unix, a copy
/// elsewhere. A copy opens the destination for write, which races a sibling
/// test's exec of it (`ETXTBSY`); the symlink never write-opens the inode the
/// child execs. The link points at the build artifact, so a test needing its
/// own variant must remove the link and copy in a replacement, never write
/// through the link.
#[cfg(unix)]
fn stage_double(src: &str, dst: &Path) {
    std::os::unix::fs::symlink(src, dst).unwrap();
}

#[cfg(not(unix))]
fn stage_double(src: &str, dst: &Path) {
    fs::copy(src, dst).unwrap();
}

#[test]
fn copilot_cli_setup_leaves_an_existing_user_statusline_untouched() {
    let env = Env::new("untouched");
    env.seed_settings();
    let before = env.settings();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert_eq!(env.settings(), before, "setup rewrote the settings file holding the user's statusLine");

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");
    assert_eq!(env.settings(), before, "self-heal touched the settings file holding the user's statusLine");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(env.settings(), before, "uninstall rewrote the settings file holding the user's statusLine");
}

#[test]
fn copilot_cli_teardown_with_nothing_installed_writes_no_file() {
    // A teardown that owns nothing must not create a settings file in the user's config
    // dir at all.
    let env = Env::new("empty-teardown");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall errored with nothing installed: {out}");
    assert_eq!(out, "NoOp", "a teardown with nothing of ours to undo must report no change, got {out}");
    assert!(!env.settings_path().exists(), "teardown created a settings file it had nothing to undo in");
}

/// Mirrors `fake_copilot.rs`'s own `CALL_LOG_FILENAME`, for the same reason the state
/// file's name is duplicated above.
const FAKE_COPILOT_CALL_LOG: &str = "fake-copilot-calls.log";

/// The fixture plugin's own name, which is also its data-root directory.
const PLUGIN_NAME: &str = "ez-fixture-plugin";

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

#[test]
fn copilot_cli_reinstalls_a_same_version_tree_change_and_nothing_else() {
    // copilot's install copy is keyed on the plugin, its freshness on the version, so a
    // tree edited at an unchanged version is invisible to every version comparison the
    // registry offers and copilot keeps serving the bytes it first copied. Only a
    // reinstall replaces that copy, and the registry cannot show one — uninstall +
    // install of the same id leaves the state that was already there — so the double's
    // call log is the observation.
    let env = Env::new("tree-refresh");
    let src = env.root.join("src-plugin");
    copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &src);
    let source = src.to_str().unwrap().to_string();
    let hello = src.join("commands").join("hello.md");
    let staged = env.data.join(PLUGIN_NAME).join("current@copilot-cli").join("commands").join("hello.md");
    let calls = || fs::read_to_string(env.copilot_home.join(FAKE_COPILOT_CALL_LOG)).unwrap_or_default();
    let uninstalls = |log: &str| log.lines().filter(|l| l.starts_with("plugin uninstall")).count();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok, "first setup failed: {out}");
    assert_eq!(out, "Installed", "first setup did not install");
    let after_install = calls();

    // An unchanged tree must not churn: without this leg the refresh below passes for
    // any binary that simply reinstalls every session.
    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok, "second setup failed: {out}");
    assert_eq!(out, "NoOp", "an unchanged tree at an unchanged version must converge to a no-op");
    assert_eq!(uninstalls(&calls()), uninstalls(&after_install), "an unchanged tree must not reinstall:\n{}", calls());

    let original = fs::read_to_string(&hello).unwrap();
    fs::write(&hello, format!("{original}\n<!-- same-version-edit -->\n")).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli", "--path", &source]);
    assert!(ok, "third setup failed: {out}");
    let staged_body = fs::read_to_string(&staged).unwrap();
    assert!(staged_body.contains("same-version-edit"), "the edit never reached the staged tree:\n{staged_body}");
    assert_eq!(uninstalls(&calls()), uninstalls(&after_install) + 1, "a changed tree must reinstall exactly once:\n{}", calls());
    assert_eq!(out, "Repaired", "a re-copied tree is a repair, not a no-op");
}

#[test]
fn copilot_cli_re_hands_the_tree_to_an_unowned_install() {
    // The same class claude carries: the tree hash lives in the marker, so an install
    // this binary never made leaves copilot's copy unaccounted for while the registry
    // entry still reads correct. Adopting on that read alone stamps ownership over bytes
    // nothing ever compared, and the (marker present, Healthy) row no-ops over them from
    // then on.
    let env = Env::new("unowned");
    let calls = || fs::read_to_string(env.copilot_home.join(FAKE_COPILOT_CALL_LOG)).unwrap_or_default();
    let uninstalls = |log: &str| log.lines().filter(|l| l.starts_with("plugin uninstall")).count();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "setup did not install");
    let after_install = calls();

    fs::remove_dir_all(env.data.join(PLUGIN_NAME).join("markers")).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed on an unowned install: {out}");
    assert_eq!(out, "Repaired", "an unowned install must be re-handed its tree, got {out}");
    // Copilot's copy is version-keyed the same way CC's is, so the uninstall half is
    // what proves the tree was handed over rather than merely re-registered.
    assert_eq!(uninstalls(&calls()), uninstalls(&after_install) + 1, "the takeover never re-handed the tree:\n{}", calls());

    // The takeover records the tree, so the next session no-ops.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "the settling heal failed: {out}");
    assert_eq!(out, "NoOp", "a recorded takeover must converge to a no-op");
}

// --- instructions ------------------------------------------------------------

/// The fixture's `<name>@<marketplace>` plugin id; the derive defaults `marketplace`
/// to `name` when no attr names it.
const INSTR_ID: &str = "ez-fixture-plugin@ez-fixture-plugin";
const INSTR_GUIDANCE: &str = "ez-fixture always-loaded guidance line.\n";

/// The exact tagged block the instructions surface must render into
/// `$COPILOT_HOME/copilot-instructions.md`: the host guidance between two html-comment
/// markers naming the plugin id, the whole block replaceable byte-for-byte.
fn instr_block() -> String {
    format!("<!-- agentgear:{INSTR_ID} begin -->\n{INSTR_GUIDANCE}<!-- agentgear:{INSTR_ID} end -->\n")
}

/// The user-scope instructions lifecycle in copilot's `copilot-instructions.md` (the
/// CLI lists it as a `home` custom-instruction file and ships it in the system prompt
/// with html comments intact): install appends our block after the user's content with
/// one blank separator line; a drifted or deleted block is repaired by self-heal;
/// uninstall strips exactly our block and the separator.
#[test]
fn copilot_cli_instructions_block_lifecycle() {
    let env = Env::new("instr-lifecycle");
    let instructions = env.copilot_home.join("copilot-instructions.md");
    let seed = "user instruction one\nuser instruction two\n";
    fs::write(&instructions, seed).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");
    let installed = format!("{seed}\n{}", instr_block());
    assert_eq!(fs::read_to_string(&instructions).unwrap(), installed, "install must append exactly our block after a blank line");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");
    assert_eq!(fs::read_to_string(&instructions).unwrap(), installed, "a no-op setup must not rewrite copilot-instructions.md");

    let tampered = format!("{seed}\n<!-- agentgear:{INSTR_ID} begin -->\ntampered\n<!-- agentgear:{INSTR_ID} end -->\n");
    fs::write(&instructions, &tampered).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the drifted block: {out}");
    assert_eq!(fs::read_to_string(&instructions).unwrap(), installed, "self-heal must restore the exact block bytes");

    fs::write(&instructions, seed).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the deleted block: {out}");
    assert_eq!(fs::read_to_string(&instructions).unwrap(), installed, "self-heal must re-add the block");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(fs::read_to_string(&instructions).unwrap(), seed, "uninstall must restore the user's file byte-for-byte");
}

/// With no pre-existing `copilot-instructions.md`, install creates the file holding
/// only our block, and uninstall — our removal being what emptied it — takes the file
/// with it.
#[test]
fn copilot_cli_instructions_owns_only_its_file() {
    let env = Env::new("instr-owned-file");
    let instructions = env.copilot_home.join("copilot-instructions.md");
    assert!(!instructions.exists(), "precondition: no copilot-instructions.md seeded");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(
        fs::read_to_string(&instructions).unwrap(),
        instr_block(),
        "a file we created holds exactly our block, no leading blank line"
    );

    // The Ok line pins the `instructions.is_some()` gate, the push and the user-scope path it is handed.
    let (ok, out) = env.fixture(&["doctor"]);
    assert!(ok, "doctor failed over a healthy install: {out}");
    let want = format!("[ ok ] guidance block present: block present in {}", instructions.display());
    assert!(out.lines().any(|l| l == want), "the doctor report lacks an ok guidance-block check over a healthy install:\n{out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert!(!instructions.exists(), "uninstall must remove the file our removal emptied");
}

/// A rival agentgear host's block (different plugin id in the markers) and the user's
/// own lines must survive our install and uninstall byte-identical: the marker id
/// scopes ownership, so two agentgear plugins coexist in one instructions file.
#[test]
fn copilot_cli_never_clobbers_a_rival_instructions_block() {
    let env = Env::new("instr-rival");
    let instructions = env.copilot_home.join("copilot-instructions.md");
    let rival =
        "user line\n<!-- agentgear:rival-plugin@rival-mkt begin -->\nrival guidance\n<!-- agentgear:rival-plugin@rival-mkt end -->\n";
    fs::write(&instructions, rival).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(
        fs::read_to_string(&instructions).unwrap(),
        format!("{rival}\n{}", instr_block()),
        "install must append our block, leaving the rival's untouched"
    );

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(
        fs::read_to_string(&instructions).unwrap(),
        rival,
        "uninstall must strip only our block, keeping the rival's and the user's"
    );
}

/// copilot also loads the modular `instructions/**/*.instructions.md` tree, but that is
/// the user's own layout — we own one block in the single home file, never a file the
/// user may be hand-managing. Install must leave that tree unwritten.
#[test]
fn copilot_cli_instructions_leaves_the_modular_tree_unwritten() {
    let env = Env::new("instr-modular");

    let (ok, out) = env.fixture(&["setup", "--agent", "copilot-cli"]);
    assert!(ok, "setup failed: {out}");
    assert!(env.copilot_home.join("copilot-instructions.md").exists(), "precondition: our block landed");

    let modular = env.copilot_home.join("instructions");
    assert!(!modular.exists(), "the modular instructions tree is the user's, never ours to create: {}", modular.display());
}
