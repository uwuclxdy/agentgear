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
//! | present| Absent, marker mid-reinstall | reconcile + write marker (our own uninstall never finished) |
//! | present| Absent | clear marker, no-op (never resurrect) |
//! | present| Disabled | no-op (never re-enable) |
//! | present| Healthy | no-op |
//! | present| NeedsRepair | reconcile + write marker (repair/update) |
//!
//! The plugin-native backends record the staged tree's hash in that marker, so with no
//! marker `claude` and `copilot-cli` reach the adopt row only where nothing needs the
//! hash: a github source, or an install strictly newer than this binary. The two
//! backends part company there: `copilot-cli` freezes every strictly-newer install,
//! `claude` freezes only one another binary of the same tool owns, so a github
//! registration ahead of the binary takes the repair row instead of adopting. At
//! this binary's own version under a local source the tree the harness holds is
//! unaccounted for, `probe` classifies `NeedsRepair`, and the heal re-hands the tree as
//! a `Repaired`. A config-family backend compares its own rendered config and adopts.
//!
//! The restart-pending flag is Claude-only: CC has no mid-session hot-reload, so a
//! repair strands the running session; config-family harnesses re-read each session and
//! need no nag. Every CC reconcile above keys the flag on its own outcome, a takeover of
//! an unowned install included: changed something sets it, changed nothing clears it.
//! What strands the session is the tree moving under it, never whose install it was, so
//! a changed outcome sets the flag under every firing event.
//!
//! Retiring a standing notice is not unconditional: only a `SessionStart` that began by
//! loading the plugin tree from disk (`startup`/`resume`, see
//! [`SessionSource::loaded_plugin_contents`]) proves the running session holds the
//! current tree. `/compact` and `/clear` fire inside the session that loaded the plugin
//! at startup — CC memoizes plugin hook registration per session — so a heal under them
//! leaves the notice standing; a `fork` is excluded too, because an in-process fork
//! inherits that stale registration and the payload does not distinguish it (the notice
//! outliving a fork costs one session). Of the two rows that run no reconcile, the healthy
//! row clears the flag only under a retiring event; the clean-uninstall row clears it under
//! any event, the plugin being gone, so the notice is moot. The disabled row leaves it
//! alone, since it converges nothing either way.

use crate::agents::{AgentBackend, BackendState};
use crate::error::Result;
use crate::host::{
    AgentReport, AgentStatus, Capabilities, Desired, HealNeededReport, HealNeededVerdict, Outcome, Plugin, Scope, SkipReason, Source,
};
use crate::install::resolve;
use crate::session::SessionSource;
use crate::{lock, restart, stamp};

pub(crate) fn self_heal_report(plugin: &Plugin, source: Source, session: SessionSource) -> Result<AgentReport> {
    // The SessionStart hook targets user-scoped installs; project scope has no
    // stable session context to key on in v1.
    let scope = Scope::User;
    let _lock = lock::acquire()?;
    // Like the lock, a missing data root fails every agent's marker read/write
    // identically: a whole-call precondition, not a per-agent failure.
    crate::install::data_dir_precondition(plugin)?;

    let retire_notice = session.loaded_plugin_contents();
    let mut report = AgentReport::new();
    for id in plugin.agents {
        report.push(id, heal_status(plugin, &source, &scope, id, retire_notice));
    }
    Ok(report)
}

/// One agent's heal slice; a failure here becomes its `Failed` entry and never
/// aborts the fan-out (a session-start heal must not let one broken agent stop
/// the other 24 from healing).
fn heal_status(plugin: &Plugin, source: &Source, scope: &Scope, id: &'static str, retire_notice: bool) -> AgentStatus {
    let backend = match resolve(id) {
        Ok(backend) => backend,
        Err(e) => return AgentStatus::Failed(e.to_string()),
    };
    if !backend.detect() {
        // a tool that isn't installed has nothing to heal
        return AgentStatus::Skipped(SkipReason::NotDetected);
    }
    // No user-scope surface (e.g. a repo-config-only IDE backend); checked before
    // the marker read so a scope-unsupported backend reads as a skip, never as a
    // marker error (heal_needed_status mirrors the order).
    if scope_unsupported(&backend.capabilities(), scope) {
        return AgentStatus::Skipped(SkipReason::ScopeUnsupported);
    }
    // This agent's OWN marker settles its source (never a sibling's — the
    // per-agent-marker invariant); `source` is the caller's `DEFAULT_SOURCE`,
    // used only absent a persisted `--path` for this specific agent.
    let marker = match stamp::read(plugin, scope, id) {
        Ok(marker) => marker,
        Err(e) => return AgentStatus::Failed(e.to_string()),
    };
    let resolved_source = stamp::source_from_marker(marker.as_ref(), source.clone());
    // The source gate needs the marker-resolved source; skip_reason re-checks the
    // scope gate (a no-op here) and applies the github-source gate.
    if let Some(reason) = skip_reason(&backend.capabilities(), scope, &resolved_source) {
        return AgentStatus::Skipped(reason);
    }
    let interrupted = marker.as_ref().is_some_and(|m| m.reinstalling);
    match heal_agent(&*backend, plugin, resolved_source, scope, marker.is_some(), interrupted, retire_notice) {
        Ok(outcome) => AgentStatus::Converged(outcome),
        Err(e) => AgentStatus::Failed(e.to_string()),
    }
}

/// Drive one detected backend's marker × `probe()` table. `resolved_source` is
/// the caller-resolved per-agent source and `has_marker` its marker's presence
/// (the caller reads it once for the source resolution and the table both).
/// `retire_notice` says the firing `SessionStart` began by loading plugin
/// contents, the only event that may retire a standing restart notice;
/// convergence delegates to the backend's `reconcile`.
fn heal_agent(
    backend: &dyn AgentBackend, plugin: &Plugin, resolved_source: Source, scope: &Scope, has_marker: bool, interrupted: bool,
    retire_notice: bool,
) -> Result<Outcome> {
    // The backend came from `resolve(id)` for this row, so its own id is the row's
    // id; the restart flag is Claude-only.
    let is_claude = backend.id() == "claude";
    // Probe against the SAME source `reconcile` (below) will render from, so a
    // `--path` install whose tree differs from the embedded blob does not read as
    // perpetual drift (probe wanting embedded bytes, reconcile writing path bytes).
    let state = backend.probe(plugin, scope, &resolved_source)?;
    // self_heal never re-enables a deliberate disable (the design's install-only
    // enable flip); adopt/repair both converge without touching enable state.
    let desired = Desired { source: resolved_source, reenable: false };

    match (has_marker, state) {
        (false, BackendState::Absent) => Ok(Outcome::NoOp),

        (false, BackendState::Healthy) => {
            // Adopt a healthy pre-existing install: converge (a no-op here), record
            // ownership.
            let outcome = backend.reconcile(plugin, &desired, scope)?;
            flag_restart(plugin, is_claude, retire_notice, &outcome);
            stamp::write(plugin, scope, &desired.source, backend.id())?;
            Ok(match outcome {
                Outcome::NoOp => Outcome::Adopted,
                other => other,
            })
        }

        (false, BackendState::Disabled | BackendState::NeedsRepair) => {
            // Take over a disabled/broken install: reconcile (leaves a disable alone,
            // repairs a break), record ownership.
            let outcome = backend.reconcile(plugin, &desired, scope)?;
            flag_restart(plugin, is_claude, retire_notice, &outcome);
            stamp::write(plugin, scope, &desired.source, backend.id())?;
            Ok(outcome)
        }

        // A reinstall this crate began and never finished: the plugin is absent because
        // WE removed it, so the clean-uninstall arm below would read our own interrupted
        // write as the user's choice and forget it for good. Repair instead, and let a
        // repair that fails again stay loud rather than settling into a no-op.
        (true, BackendState::Absent) if interrupted => {
            let outcome = backend.reconcile(plugin, &desired, scope)?;
            flag_restart(plugin, is_claude, retire_notice, &outcome);
            stamp::write(plugin, scope, &desired.source, backend.id())?;
            Ok(outcome)
        }

        (true, BackendState::Absent) => {
            // Clean uninstall under our marker: forget it, do not reinstall. The
            // notice is cleared under any event: the plugin is gone, so a session
            // still holding an older tree has nothing left to restart into.
            if is_claude {
                let _ = restart::clear(plugin);
            }
            stamp::clear(plugin, scope, backend.id())?;
            Ok(Outcome::Cleared)
        }

        (true, BackendState::Disabled) => Ok(Outcome::NoOp), // never re-enable a deliberate disable

        (true, BackendState::Healthy) => {
            // Healthy + current: a session that began by loading plugin contents
            // already holds this tree, so a prior restart-pending flag no longer
            // applies. `/compact` and `/clear` fire inside the session still
            // holding the old tree, so under those the notice must stand.
            if is_claude && retire_notice {
                let _ = restart::clear(plugin);
            }
            // A hook killed after `plugin_install` but before `record_converged`
            // leaves `reinstalling` beside a healthy row; end it (without touching
            // the tree hash), or a later manual uninstall reads the
            // interrupted-repair row and resurrects the install.
            #[cfg(any(feature = "claude", feature = "copilot-cli"))]
            if interrupted {
                stamp::record_converged(plugin, scope, &desired.source, backend.id(), None)?;
            }
            Ok(Outcome::NoOp)
        }

        (true, BackendState::NeedsRepair) => {
            let outcome = backend.reconcile(plugin, &desired, scope)?;
            flag_restart(plugin, is_claude, retire_notice, &outcome);
            stamp::write(plugin, scope, &desired.source, backend.id())?;
            Ok(outcome)
        }
    }
}

/// What a CC reconcile did to the running session, recorded for the host's own notice
/// hook. A reconcile that changed something handed CC a tree the session cannot reload,
/// so it strands, whatever event fired; one that changed nothing means the running
/// session already holds what CC holds, which retires any flag an earlier session left —
/// but only when the firing event began by loading plugin contents (`retire_notice`),
/// since `/compact` and `/clear` run inside the session that still holds the old tree.
/// Whose install it was never enters it: a takeover that re-hands the tree strands the
/// session exactly like an owned repair. Best-effort both ways, so a flag write cannot
/// fail a heal that otherwise succeeded.
fn flag_restart(plugin: &Plugin, is_claude: bool, retire_notice: bool, outcome: &Outcome) {
    if !is_claude {
        return;
    }
    if *outcome != Outcome::NoOp {
        let _ = restart::set(plugin);
    } else if retire_notice {
        let _ = restart::clear(plugin);
    }
}

/// Spawn-free fan-out mirror of [`self_heal_report`]: one [`HealNeededVerdict`]
/// per configured agent, answered from files alone (and `which`-based detection),
/// so a host's hook can run the CLI only when a heal would act. No lock, no
/// data-root precondition, no CLI spawn — every "cannot tell" reads as `Needed`,
/// the conservative answer that changes nothing about what `self_heal` would do.
/// The firing `session` gates the same way it does in the heal: a standing
/// restart notice counts as a change only when the event may retire it.
pub(crate) fn heal_needed_report(plugin: &Plugin, source: Source, session: SessionSource) -> Result<HealNeededReport> {
    let retire_notice = session.loaded_plugin_contents();
    let mut report = HealNeededReport::new();
    for id in plugin.agents {
        report.push(id, heal_needed_status(plugin, &source, id, retire_notice));
    }
    Ok(report)
}

/// The two spawn-free skip gates [`heal_status`] applies (and
/// [`heal_needed_status`] mirrors): a backend with no surface at `scope`, and a
/// github source on a config-merge backend (no plugin-native surface to hand a
/// ref to). Split out so the classifier's mirror is unit-testable — a github
/// source cannot be reached through the embedded-default host fixture, and
/// `detect()` is env-dependent.
fn skip_reason(capabilities: &Capabilities, scope: &Scope, source: &Source) -> Option<SkipReason> {
    if scope_unsupported(capabilities, scope) {
        return Some(SkipReason::ScopeUnsupported);
    }
    if matches!(source, Source::GitHub { .. }) && !capabilities.plugins {
        return Some(SkipReason::SourceUnsupported);
    }
    None
}

/// Whether this backend has no surface at `scope` — the first skip gate, run
/// before the marker read in both `heal_status` and `heal_needed_status` so a
/// scope-unsupported backend reads as a skip, never as a marker error.
fn scope_unsupported(capabilities: &Capabilities, scope: &Scope) -> bool {
    !capabilities.scopes.contains(&scope.as_cli())
}

fn heal_needed_status(plugin: &Plugin, source: &Source, id: &'static str, retire_notice: bool) -> HealNeededVerdict {
    let backend = match resolve(id) {
        Ok(backend) => backend,
        Err(e) => return HealNeededVerdict::Needed { reason: format!("backend unavailable: {e}") },
    };
    if !backend.detect() {
        return HealNeededVerdict::Skipped(SkipReason::NotDetected);
    }
    let scope = Scope::User;
    // Scope gate before the marker read, exactly as in heal_status.
    if scope_unsupported(&backend.capabilities(), &scope) {
        return HealNeededVerdict::Skipped(SkipReason::ScopeUnsupported);
    }
    let marker = match stamp::read(plugin, &scope, id) {
        Ok(marker) => marker,
        Err(e) => return HealNeededVerdict::Needed { reason: format!("cannot tell: {e}") },
    };
    let resolved_source = stamp::source_from_marker(marker.as_ref(), source.clone());
    // Mirror heal_status's two spawn-free skip gates, so a host with such an
    // agent is not pinned to the two-spawn floor self_heal itself skips.
    if let Some(reason) = skip_reason(&backend.capabilities(), &scope, &resolved_source) {
        return HealNeededVerdict::Skipped(reason);
    }
    // Every non-claude backend has no spawn-free check; answering `Needed` keeps
    // its current behavior — the host runs self_heal, which does what it did.
    if id != "claude" {
        return HealNeededVerdict::Needed { reason: "no spawn-free check".into() };
    }
    #[cfg(feature = "claude")]
    {
        crate::agents::claude::heal_needed(plugin, &scope, source, retire_notice)
    }
    #[cfg(not(feature = "claude"))]
    {
        let _ = retire_notice;
        HealNeededVerdict::Needed { reason: "claude backend not compiled in".into() }
    }
}

#[cfg(test)]
#[path = "../tests/unit/selfheal.rs"]
mod selfheal_tests;
