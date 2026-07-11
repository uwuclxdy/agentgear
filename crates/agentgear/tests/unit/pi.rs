//! pi backend unit tests: pi is a detect-only, no-surface harness, so reconcile
//! and remove are no-ops and probe is always Healthy (a present marker must never
//! be dropped for a host that has pi). Detection resolves through an env override
//! or HOME — exercised here via the pure `resolve_pi_dir` so no process-global env
//! is mutated (unsafe in edition 2024; the crate forbids it regardless).

use std::ffi::OsStr;
use std::path::Path;

use super::{PiBackend, resolve_pi_dir};
use crate::agents::{AgentBackend, BackendState};
use crate::host::{Desired, Outcome, Plugin, Scope, Source};

fn plugin() -> Plugin {
    // `blob` is unused: pi writes nothing and probe ignores the plugin entirely.
    Plugin { name: "ez-fixture", marketplace: "ez-mkt", version: "0.1.0", agents: &["pi"], blob: &[] }
}

#[test]
fn env_override_redirects_detection_over_home() {
    // A `PI_CODING_AGENT_DIR` value points at pi's config root and wins over HOME.
    let home = Path::new("/home/user");
    assert_eq!(resolve_pi_dir(Some(OsStr::new("/some/pi/root")), Some(home)).as_deref(), Some(Path::new("/some/pi/root")),);
}

#[test]
fn empty_or_absent_override_falls_back_to_home_dot_pi() {
    let home = Path::new("/home/user");
    // An empty env value is ignored; detection falls back to `~/.pi`.
    assert_eq!(resolve_pi_dir(Some(OsStr::new("")), Some(home)), Some(home.join(".pi")));
    assert_eq!(resolve_pi_dir(None, Some(home)), Some(home.join(".pi")));
    // No override and no home means no detectable dir.
    assert_eq!(resolve_pi_dir(None, None), None);
}

#[test]
fn detection_gate_follows_the_resolved_dir() {
    // The `.is_dir()` gate detect() applies over the resolved dir: an existing
    // override dir means "pi present here", a missing path does not.
    let present = std::env::temp_dir().join(format!("ez-pi-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&present).unwrap();
    assert!(
        resolve_pi_dir(Some(present.as_os_str()), None).as_deref().is_some_and(Path::is_dir),
        "an existing override dir gates detection on",
    );

    let missing = present.join("nope");
    assert!(!resolve_pi_dir(Some(missing.as_os_str()), None).as_deref().is_some_and(Path::is_dir));

    std::fs::remove_dir_all(&present).ok();
}

#[test]
fn probe_is_always_healthy() {
    // No surface can drift, so probe never returns Absent (which would drop a
    // present marker in self_heal).
    let state = PiBackend.probe(&plugin(), &Scope::User).unwrap();
    assert!(matches!(state, BackendState::Healthy));
}

#[test]
fn reconcile_and_remove_are_noops() {
    let desired = Desired { source: Source::Embedded, reenable: true };
    assert_eq!(PiBackend.reconcile(&plugin(), &desired, &Scope::User).unwrap(), Outcome::NoOp);
    assert_eq!(PiBackend.remove(&plugin(), &Scope::User).unwrap(), Outcome::NoOp);
}

#[test]
fn capabilities_declare_no_writable_surface() {
    let caps = PiBackend.capabilities();
    assert!(!caps.mcp, "pi has no native mcp surface");
    assert!(!caps.hooks, "pi has no config-file hook surface");
    assert!(!caps.plugins);
    assert_eq!(caps.scopes, &["user"]);
}
