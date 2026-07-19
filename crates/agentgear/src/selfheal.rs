//! The SessionStart entrypoint. The hook is part of the plugin, so it only ever
//! fires on an install that already exists: self_heal repairs *broken* installs,
//! never resurrects an *absent* one (a resurrected uninstall reads as adware).
//!
//! Fans out over the plugin's configured agents (user scope only — project scope
//! has no stable session context to key on in v1). Each *detected* backend's
//! per-agent marker × `probe()` state drives the table (design §5):
//!
//! | marker | state | action |
//! |--------|-------|--------|
//! | absent | Absent | no-op |
//! | absent | Healthy | adopt: reconcile + write marker (no-op maps to `Adopted`) |
//! | absent | Disabled/NeedsRepair | reconcile + write marker (its own outcome) |
//! | present| Absent | clear marker, no-op (never resurrect) |
//! | present| Disabled | no-op (never re-enable) |
//! | present| Healthy | no-op |
//! | present| NeedsRepair | reconcile + write marker (repair/update) |
//!
//! The restart-pending flag is Claude-only: CC has no mid-session hot-reload, so a
//! repair strands the running session; config-family harnesses re-read each
//! session and need no nag. A healthy, adopted, or cleanly-removed CC install
//! clears it; a CC repair that changed something sets it.

use crate::agents::{AgentBackend, BackendState};
use crate::error::Result;
use crate::host::{AgentReport, AgentStatus, Desired, Outcome, Plugin, Scope, SkipReason, Source};
use crate::install::resolve;
use crate::{lock, restart, stamp};

pub(crate) fn self_heal_report(plugin: &Plugin, source: Source) -> Result<AgentReport> {
    // The SessionStart hook targets user-scoped installs; project scope has no
    // stable session context to key on in v1.
    let scope = Scope::User;
    let _lock = lock::acquire()?;
    // Like the lock, a missing data root fails every agent's marker read/write
    // identically: a whole-call precondition, not a per-agent failure.
    crate::install::data_dir_precondition(plugin)?;

    let mut report = AgentReport::new();
    for id in plugin.agents {
        report.push(id, heal_status(plugin, &source, &scope, id));
    }
    Ok(report)
}

/// One agent's heal slice; a failure here becomes its `Failed` entry and never
/// aborts the fan-out (a session-start heal must not let one broken agent stop
/// the other 24 from healing).
fn heal_status(plugin: &Plugin, source: &Source, scope: &Scope, id: &'static str) -> AgentStatus {
    let backend = match resolve(id) {
        Ok(backend) => backend,
        Err(e) => return AgentStatus::Failed(e.to_string()),
    };
    if !backend.detect() {
        // a tool that isn't installed has nothing to heal
        return AgentStatus::Skipped(SkipReason::NotDetected);
    }
    if !backend.capabilities().scopes.contains(&scope.as_cli()) {
        // no user-scope surface (e.g. a repo-config-only IDE backend)
        return AgentStatus::Skipped(SkipReason::ScopeUnsupported);
    }
    match heal_agent(&*backend, plugin, source, scope, id == "claude") {
        Ok(outcome) => AgentStatus::Converged(outcome),
        Err(e) => AgentStatus::Failed(e.to_string()),
    }
}

/// Drive one detected backend's marker × `probe()` table. `is_claude` gates the
/// CC-only restart flag; convergence delegates to the backend's `reconcile`.
fn heal_agent(backend: &dyn AgentBackend, plugin: &Plugin, source: &Source, scope: &Scope, is_claude: bool) -> Result<Outcome> {
    let marker = stamp::read(plugin, scope, backend.id())?;
    // This agent's OWN marker settles its source (never a sibling's — the
    // per-agent-marker invariant); `source` is the caller's `DEFAULT_SOURCE`,
    // used only absent a persisted `--path` for this specific agent.
    let resolved_source = stamp::source_from_marker(marker.as_ref(), source.clone());
    // Probe against the SAME source `reconcile` (below) will render from, so a
    // `--path` install whose tree differs from the embedded blob does not read as
    // perpetual drift (probe wanting embedded bytes, reconcile writing path bytes).
    let state = backend.probe(plugin, scope, &resolved_source)?;
    // self_heal never re-enables a deliberate disable (the design's install-only
    // enable flip); adopt/repair both converge without touching enable state.
    let desired = Desired { source: resolved_source, reenable: false };

    match (marker.is_some(), state) {
        (false, BackendState::Absent) => Ok(Outcome::NoOp),

        (false, BackendState::Healthy) => {
            // Adopt a healthy pre-existing install: converge (a no-op here), record
            // ownership; not our update, so clear any stale restart flag.
            if is_claude {
                let _ = restart::clear(plugin);
            }
            let outcome = backend.reconcile(plugin, &desired, scope)?;
            stamp::write(plugin, scope, &desired.source, backend.id())?;
            Ok(match outcome {
                Outcome::NoOp => Outcome::Adopted,
                other => other,
            })
        }

        (false, BackendState::Disabled | BackendState::NeedsRepair) => {
            // Adopt a disabled/broken install: reconcile (leaves a disable alone,
            // repairs a break), record ownership; not our update -> clear the flag.
            if is_claude {
                let _ = restart::clear(plugin);
            }
            let outcome = backend.reconcile(plugin, &desired, scope)?;
            stamp::write(plugin, scope, &desired.source, backend.id())?;
            Ok(outcome)
        }

        (true, BackendState::Absent) => {
            // Clean uninstall under our marker: forget it, do not reinstall.
            if is_claude {
                let _ = restart::clear(plugin);
            }
            stamp::clear(plugin, scope, backend.id())?;
            Ok(Outcome::Cleared)
        }

        (true, BackendState::Disabled) => Ok(Outcome::NoOp), // never re-enable a deliberate disable

        (true, BackendState::Healthy) => {
            // Healthy + current: a fresh session already loaded this plugin, so a
            // prior restart-pending flag no longer applies. Best-effort clear.
            if is_claude {
                let _ = restart::clear(plugin);
            }
            Ok(Outcome::NoOp)
        }

        (true, BackendState::NeedsRepair) => {
            let outcome = backend.reconcile(plugin, &desired, scope)?;
            // A repair re-materialized the plugin; the running CC session is now stale.
            if is_claude && outcome != Outcome::NoOp {
                let _ = restart::set(plugin);
            }
            stamp::write(plugin, scope, &desired.source, backend.id())?;
            Ok(outcome)
        }
    }
}
