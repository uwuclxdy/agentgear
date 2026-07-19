//! Hermetic proof that `update`/`self_heal` never broadcast one agent's own
//! persisted `Source::Path` onto a sibling agent installed from a different
//! source — the stamp marker is per-`(plugin, scope, agent)`
//! (`docs/design.md`'s per-agent-marker invariant: installing into
//! `[claude, codex]` must never confuse one backend's marker for another's), so
//! resolving it back into a runtime `Source` must be too.
//!
//! No docker, no CLI: codex and cline both detect off a pre-created home dir
//! alone, and `claude` stays undetected (curated PATH), so the fan-out this test
//! drives is a pure two-non-CC-backend exercise.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

struct Env {
    root: PathBuf,
    codex: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir: no `claude`, `codex`, `cline`,
    /// so detection rides entirely on the pre-created home dirs below.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-path-isolation-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            codex: root.join(".codex"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        fs::create_dir_all(&env.codex).unwrap();
        // cline detects off `~/.cline/data/settings` (the modern unified store) or
        // `~/Documents/Cline`; create the former, mirroring `tests/cline.rs`.
        fs::create_dir_all(env.root.join(".cline").join("data").join("settings")).unwrap();
        for dir in [&env.config, &env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
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

    fn codex_prompt(&self) -> String {
        fs::read_to_string(self.codex.join("prompts").join("ez-fixture-plugin-hello.md")).unwrap()
    }

    fn cline_workflow(&self) -> String {
        fs::read_to_string(self.root.join("Documents").join("Cline").join("Workflows").join("ez-fixture-plugin-hello.md")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory, matching `tests/codex.rs` /
/// `tests/cline.rs`'s own helper.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

/// Recursively copy a plugin tree into a scratch dir, mirroring `tests/e2e.rs`'s
/// helper of the same name: a mutable copy this test can edit without touching
/// the checked-in fixture (which other tests read).
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
fn update_never_broadcasts_one_agents_path_source_onto_a_sibling() {
    let env = Env::new("hijack");

    // codex installed from a mutable --path source.
    let src_plugin = env.root.join("src-plugin");
    copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &src_plugin);
    let (ok, out) = env.fixture(&["setup", "--agent", "codex", "--path", src_plugin.to_str().unwrap()]);
    assert!(ok, "codex path setup failed: {out}");
    assert_eq!(out, "Installed", "codex path install did not register");

    // cline installed from the default (embedded) source — a different agent, a
    // different source, same plugin + scope.
    let (ok, out) = env.fixture(&["setup", "--agent", "cline"]);
    assert!(ok, "cline setup failed: {out}");
    assert_eq!(out, "Installed", "cline embedded install did not register");

    // Mutate codex's path source after both installs: a marker only that tree
    // carries, so any render that picks it up (correctly for codex, wrongly for
    // cline) is provable by content inspection alone.
    let hello = src_plugin.join("commands/hello.md");
    let original = fs::read_to_string(&hello).unwrap();
    fs::write(&hello, format!("{original}\n<!-- path-source-marker -->\n")).unwrap();

    // An unfiltered update fans out over every detected agent (codex + cline
    // here). Pre-fix, source resolution scanned ALL agents for the first
    // path-mode marker (codex's, since codex precedes cline in `plugin.agents`)
    // and broadcast it to every agent, cline included.
    let (ok, out) = env.fixture(&["update"]);
    assert!(ok, "update failed: {out}");

    // codex re-materializes from ITS OWN path source: the mutation shows up.
    let codex_prompt = env.codex_prompt();
    assert!(codex_prompt.contains("path-source-marker"), "codex should track its own persisted --path source:\n{codex_prompt}");

    // cline must NOT pick up codex's path source: its rendered workflow must stay
    // the embedded content, with no trace of codex's mutation.
    let cline_workflow = env.cline_workflow();
    assert!(
        !cline_workflow.contains("path-source-marker"),
        "cline was hijacked onto codex's --path source (per-agent-marker invariant broken):\n{cline_workflow}"
    );
}

/// Uninstall's strip-set renders from the agent's OWN marker-rehydrated `--path`
/// tree, not the baked embedded blob: a command that exists ONLY in the path
/// tree still gets its translation deleted. Pre-fix, `remove()` hardcoded
/// `Source::Embedded`, so it never knew the path-only command's name and left
/// its rendered file orphaned in `~/.codex/prompts/`.
#[test]
fn uninstall_strips_from_the_agents_own_path_source() {
    let env = Env::new("path-uninstall");

    // A path tree that DIVERGES from the embedded one by a whole component: an
    // extra command name the embedded blob has never heard of.
    let src_plugin = env.root.join("src-plugin");
    copy_dir_all(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &src_plugin);
    fs::write(src_plugin.join("commands").join("extra.md"), "path-tree-only command body\n").unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "codex", "--path", src_plugin.to_str().unwrap()]);
    assert!(ok, "codex path setup failed: {out}");
    let extra_prompt = env.codex.join("prompts").join("ez-fixture-plugin-extra.md");
    assert!(extra_prompt.exists(), "the path-only command must have been rendered: {}", extra_prompt.display());

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert!(
        !extra_prompt.exists(),
        "uninstall must strip the path-only command by rendering codex's own path source, not the embedded blob"
    );
    assert!(!env.codex.join("prompts").join("ez-fixture-plugin-hello.md").exists(), "the shared command must be stripped too");
}
