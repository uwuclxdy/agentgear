//! The pi backend — detect-only. Mario Zechner's "pi" coding agent
//! (`@earendil-works/pi-coding-agent`, binary `pi`) exposes no config-file surface
//! agentgear can safely translate into: core pi ships NO native mcp (only optional
//! third-party TS extensions do, each with its own convention) and NO config-file
//! hooks (its "hooks" are in-process TypeScript extension handlers, not a writable
//! table). Its file-writable surfaces — prompt-template slash commands and skills —
//! are out of this backend's scope. So this backend only *detects* pi: reconcile
//! and remove are no-ops and probe is always Healthy, so a present marker is never
//! dropped for a host that has pi installed. Full rationale + sources:
//! `docs/harness/pi.md`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{AgentBackend, BackendState};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::Result;
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct PiBackend;

impl AgentBackend for PiBackend {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn detect(&self) -> bool {
        // `pi` on PATH is the direct signal; the config dir is the fallback. Both
        // the env override and HOME resolve through `pi_dir`, so a test redirecting
        // `PI_CODING_AGENT_DIR` or `HOME` also redirects detection.
        which::which("pi").is_ok() || pi_dir().is_some_and(|d| d.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // Honest: pi exposes no surface this backend writes. Every flag is false and
        // the only scope we'd ever key on is user (project `.pi` is trust-gated and
        // we write nothing regardless).
        Capabilities {
            plugins: false,
            mcp: false,
            hooks: false,
            commands: false,
            agents: false,
            skills: false,
            instructions: false,
            scopes: &["user"],
        }
    }

    fn probe(&self, _plugin: &Plugin, _scope: &Scope, _source: &Source) -> Result<BackendState> {
        // Nothing is ever written, so nothing can drift or break -> always Healthy.
        // An Absent here would make self_heal drop a present marker for a host where
        // pi is installed; Healthy keeps it (never-resurrect and never-re-enable both
        // reduce to a NoOp against a Healthy state).
        Ok(BackendState::Healthy)
    }

    fn reconcile(&self, _plugin: &Plugin, _desired: &Desired, _scope: &Scope) -> Result<Outcome> {
        // No writable surface: the converged state is the empty state.
        Ok(Outcome::NoOp)
    }

    fn remove(&self, _plugin: &Plugin, _scope: &Scope, _source: &Source) -> Result<Outcome> {
        // Nothing was ever written, so there is nothing to undo.
        Ok(Outcome::NoOp)
    }

    fn report(&self, _plugin: &Plugin, _source: &Source) -> DoctorReport {
        let detected = if self.detect() {
            DoctorCheck { name: "pi detected", status: CheckStatus::Ok("`pi` on PATH or ~/.pi present".into()) }
        } else {
            DoctorCheck {
                name: "pi detected",
                status: CheckStatus::Fail {
                    problem: "pi CLI not detected".into(),
                    fix: "install it with `npm install -g --ignore-scripts @earendil-works/pi-coding-agent`".into(),
                },
            }
        };
        let surface = DoctorCheck {
            name: "pi surface",
            status: CheckStatus::Ok(
                "detect-only: no mcp or config-file hooks to translate; pi's file-writable commands/skills surfaces are out of scope"
                    .into(),
            ),
        };
        DoctorReport::from_checks(vec![detected, surface])
    }
}

// --- paths -------------------------------------------------------------------

/// A "pi is present here" directory: pi's config root, honoring
/// `PI_CODING_AGENT_DIR` (its documented override of the `~/.pi/agent` root,
/// checked first) then `~/.pi` (the reference-confirmed default detect dir). Split
/// from the env/home lookup as [`resolve_pi_dir`] so a unit test can exercise the
/// redirect without mutating process-global env.
fn pi_dir() -> Option<PathBuf> {
    resolve_pi_dir(std::env::var_os("PI_CODING_AGENT_DIR").as_deref(), dirs::home_dir().as_deref())
}

fn resolve_pi_dir(env_override: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = env_override.filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home.map(|h| h.join(".pi"))
}

#[cfg(test)]
#[path = "../../tests/unit/pi.rs"]
mod pi_tests;
