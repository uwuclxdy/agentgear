//! Hermetic pin of embedded-marker rehydration on a github-default host
//! (`embedded_github_fixture`: `embed = true`, `default_source = "github"`): an
//! explicit `install(Source::Embedded)` into gemini must stay embedded through
//! `self_heal_report` (healthy, never the github-source skip) and
//! `uninstall_report` (config entries stripped and marker cleared, never
//! skip-and-orphan). Same temp-`HOME` isolation as the per-backend hermetic
//! tests; nothing reaches the network, because every call here is embedded and
//! the only github-capable backend (claude) is undetected on the fixture-only
//! PATH.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_embedded_github_fixture");

struct Env {
    root: PathBuf,
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-embed-gh-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env { path: fixture_dir(), root };
        // Pre-creating ~/.gemini is all gemini's detect() needs; claude stays
        // undetected, so gemini is the lone converging agent.
        fs::create_dir_all(env.root.join(".gemini")).unwrap();
        for dir in ["config", "data", "run"] {
            fs::create_dir_all(env.root.join(dir)).unwrap();
        }
        env
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("PATH", &self.path);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn settings_path(&self) -> PathBuf {
        self.root.join(".gemini").join("settings.json")
    }

    fn settings(&self) -> String {
        fs::read_to_string(self.settings_path()).unwrap()
    }

    /// Marker files under the scratch `XDG_DATA_HOME` (`data_root` = data dir +
    /// plugin name); empty before install and after uninstall.
    fn marker_files(&self) -> Vec<PathBuf> {
        let dir = self.root.join("data").join("ez-fixture-plugin").join("markers");
        fs::read_dir(dir).map(|entries| entries.flatten().map(|e| e.path()).collect()).unwrap_or_default()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH holding only the fixture binary's dir, so no backend detects via `which`.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn embedded_install_on_a_github_default_host_stays_embedded() {
    let env = Env::new("lifecycle");

    let (ok, out) = env.fixture(&["install"]);
    assert!(ok, "embedded install failed:\n{out}");
    assert!(out.lines().any(|l| l == "gemini: installed"), "gemini must install from the embedded blob:\n{out}");
    assert!(env.settings().contains("ez-fixture"), "gemini settings.json missing our entries");

    // The stamp records the explicit embedded source, not the github default.
    let markers = env.marker_files();
    let marker_file = markers.first().expect("install must write gemini's marker");
    let marker = fs::read_to_string(marker_file).unwrap();
    assert!(marker.contains("\"source_mode\": \"embedded\""), "marker must stamp embedded:\n{marker}");

    // self_heal resolves gemini's own marker back to Embedded: a healthy no-op,
    // never the github-source skip.
    let (ok, out) = env.fixture(&["self-heal-report"]);
    assert!(ok, "self-heal failed:\n{out}");
    assert!(out.lines().any(|l| l == "gemini: no changes needed"), "a healthy embedded install must heal to a no-op:\n{out}");
    assert!(!out.contains("gemini: skipped"), "the github gate must not skip an embedded install:\n{out}");

    // Uninstall strips gemini's entries (no skip-and-orphan) and clears its marker.
    let (ok, out) = env.fixture(&["uninstall-report"]);
    assert!(ok, "uninstall failed:\n{out}");
    assert!(out.lines().any(|l| l == "gemini: removed"), "uninstall must strip the embedded install, never skip it:\n{out}");
    // The install authored this settings.json from nothing, so the uninstall takes it
    // back out rather than leaving the shells of the containers it created.
    assert!(!env.settings_path().exists(), "uninstall orphaned a settings.json it authored: {}", env.settings());
    assert!(env.marker_files().is_empty(), "uninstall must clear gemini's marker");
}
