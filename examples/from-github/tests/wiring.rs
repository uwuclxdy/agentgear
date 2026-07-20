//! Regression coverage for the zero-embed host. That this crate compiles at all is
//! the proof of the zero-embed feature pairing: the derive's `embed = false` arm
//! (an empty baked blob) against the lib built `default-features = false` (no
//! `embed` feature). If either half regressed, `cargo build -p from-github` would
//! fail before these tests ran.
//!
//! The const checks run in-process (no env, no network). The empty-blob failure and
//! the intended GitHub flow need a controlled `claude` on `PATH`, which needs a
//! spawned subprocess (the workspace `unsafe_code = "forbid"` lint bans the in-process
//! `std::env::set_var`), so those live in the `#[cfg(unix)]` module below.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agentgear::{PluginHost, Source};
use from_github::FromGithub;

#[test]
fn bakes_no_blob() {
    assert!(FromGithub::embedded_blob().is_empty(), "`embed = false` must bake an empty blob");
}

#[test]
fn default_source_is_the_version_tag() {
    // `default_source = "github"` expands to Source::GitHub with `ref_` defaulted to
    // `v{CARGO_PKG_VERSION}` (the tag `claude plugin tag` produces), so the plugin
    // tree and the binary stay version-aligned.
    let Source::GitHub { repo, ref_ } = FromGithub::DEFAULT_SOURCE else {
        panic!("`default_source = \"github\"` must expand to Source::GitHub");
    };
    assert_eq!(repo, "uwuclxdy/agentgear");
    assert_eq!(ref_, "v0.1.0-rc.1");
    assert_eq!(ref_, concat!("v", env!("CARGO_PKG_VERSION")));
}

#[cfg(unix)]
mod spawned {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const BIN: &str = env!("CARGO_BIN_EXE_from-github");
    const PLUGIN_ID: &str = "from-github@from-github";

    /// A throwaway temp home: HOME + XDG dirs redirected here, so nothing touches the
    /// developer's real config and the shared `flock` lives under the temp runtime dir.
    struct Sandbox {
        root: PathBuf,
    }

    impl Sandbox {
        fn new(name: &str) -> Self {
            // `process::id()` is constant across a binary's tests, so `name` keeps two
            // tests' roots (and their runtime-dir locks) distinct.
            let root = std::env::temp_dir().join(format!("ez-from-github-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            for dir in ["data", "run", "config", "bin"] {
                fs::create_dir_all(root.join(dir)).unwrap();
            }
            Sandbox { root }
        }

        fn apply(&self, cmd: &mut Command, path: &OsString) {
            cmd.env("HOME", &self.root)
                .env("XDG_CONFIG_HOME", self.root.join("config"))
                .env("XDG_DATA_HOME", self.root.join("data"))
                .env("XDG_RUNTIME_DIR", self.root.join("run"))
                .env("PATH", path);
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// A fake `claude` in the sandbox bin dir: answers the two `list --json` reads
    /// with `[]` and `--version` with a supported version. That is enough for the
    /// claude backend to be detected and for `reconcile` to reach `materialize`
    /// without a real CLI or any network, so the empty-blob error surfaces
    /// deterministically whether or not a real `claude` is on the dev box.
    fn write_fake_claude(root: &Path) -> OsString {
        let shim = root.join("bin").join("claude");
        let script = "#!/bin/sh\nfor a in \"$@\"; do\n  [ \"$a\" = \"--version\" ] && { echo '2.1.209 (Claude Code)'; exit 0; }\ndone\necho '[]'\nexit 0\n";
        fs::write(&shim, script).unwrap();
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
        root.join("bin").into_os_string()
    }

    #[test]
    fn embedded_source_errors_on_the_empty_blob() {
        let sb = Sandbox::new("embedded-error");
        let path = write_fake_claude(&sb.root);

        let mut cmd = Command::new(BIN);
        cmd.args(["setup", "--embedded"]);
        sb.apply(&mut cmd, &path);
        let out = cmd.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);

        assert!(!out.status.success(), "install from Source::Embedded must fail on a zero-embed host:\n{stderr}");
        // `Error::Tree` Display prefix + the shared "use Source::GitHub / enable
        // embed" hint. The exact wording differs by whether the lib's `embed` feature
        // is on (unified `--all-features` build) or off (`-p from-github`); both are
        // the same empty-blob failure and both carry these tokens.
        assert!(stderr.contains("invalid plugin tree"), "not the plugin-tree error:\n{stderr}");
        assert!(stderr.contains("embed"), "error should point at the embed feature:\n{stderr}");
    }

    /// The GitHub flow against a real `claude`: `setup` uses DEFAULT_SOURCE (the
    /// `v{version}` tag), so `ensure_marketplace` sends `{repo}@{ref_}` and the
    /// marketplace tracks the pinned tag. Proven green against the pushed version
    /// tag (+ its root `.claude-plugin/marketplace.json`) 2026-07-17.
    ///
    /// Env-gated, not merely `#[ignore]`d: the e2e CI job runs `-- --ignored`, so
    /// `#[ignore]` *selects* this test. It stays behind `AGENTGEAR_E2E_GITHUB` so the
    /// default CI run does not couple to a mutable remote tag — the assertion pins
    /// the current `v{version}`, so a workspace version bump without a matching
    /// pushed tag would red it.
    /// Run it with `AGENTGEAR_E2E_GITHUB=1` once the tag for the current version exists.
    #[test]
    #[ignore = "needs AGENTGEAR_E2E_GITHUB + network + the matching version tag pushed"]
    fn github_install_pins_the_version_tag() {
        if std::env::var_os("AGENTGEAR_E2E_GITHUB").is_none() {
            eprintln!("skipping github pin: set AGENTGEAR_E2E_GITHUB=1 (needs a tagged repo, docs/todo.md §2)");
            return;
        }
        if !claude_available() {
            eprintln!("skipping: `claude` not on PATH");
            return;
        }
        let sb = Sandbox::new("github-pin");
        let path = path_with_real_claude(&sb.root);

        // `setup` uses DEFAULT_SOURCE (the GitHub tag) — the correct call for a
        // zero-embed host, with no baked blob to materialize.
        let mut setup = Command::new(BIN);
        setup.arg("setup").env("CLAUDE_CONFIG_DIR", sb.root.join("cfg"));
        sb.apply(&mut setup, &path);
        let out = setup.output().unwrap();
        assert!(out.status.success(), "github setup failed:\n{}", String::from_utf8_lossy(&out.stderr));

        // Intended end state: the plugin is registered at the pinned tag's version.
        let mut list = Command::new("claude");
        list.args(["plugin", "list", "--json"]).env("CLAUDE_CONFIG_DIR", sb.root.join("cfg"));
        sb.apply(&mut list, &path);
        let listed = String::from_utf8_lossy(&list.output().unwrap().stdout).into_owned();
        assert!(listed.contains(PLUGIN_ID), "plugin not registered from the GitHub source:\n{listed}");
        assert!(listed.contains(env!("CARGO_PKG_VERSION")), "installed version is not the pinned version tag:\n{listed}");
    }

    fn claude_available() -> bool {
        Command::new("claude").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    /// PATH holding the sandbox bin dir plus the real `claude`'s dir, so `which`
    /// resolves the genuine CLI for the live test.
    fn path_with_real_claude(root: &Path) -> OsString {
        let mut dirs = vec![root.join("bin")];
        if let Some(path) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        std::env::join_paths(&dirs).unwrap_or_default()
    }
}
