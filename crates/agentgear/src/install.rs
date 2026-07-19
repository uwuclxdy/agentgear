//! Lifecycle orchestration: acquire the shared lock, fan out over the plugin's
//! configured agent backends — converging (or removing) each and stamping a
//! per-agent marker — collecting one [`AgentStatus`] per agent into an
//! [`AgentReport`]. The lock is held across the whole sequence so two tools
//! self-healing at once serialize. The merged-`Outcome` methods collapse the
//! report at the `PluginHost` seam.

use crate::agents::backend_for;
use crate::error::{Error, Result};
use crate::host::{AgentReport, AgentStatus, Desired, Outcome, Plugin, Scope, SkipReason, Source};
use crate::{lock, stamp};

/// Install into the subset of `plugin.agents` whose id is in `filter` (an empty
/// filter = all). Powers `PluginHost::install`/`install_into` and their
/// `*_report` variants.
pub(crate) fn install_report(plugin: &Plugin, scope: Scope, source: Source, filter: &[&str]) -> Result<AgentReport> {
    let _lock = lock::acquire()?;
    let desired = Desired { source, reenable: true };
    // A first install never sets restart-pending: it precedes any session that
    // relies on the plugin, so there is nothing stale to reload. Never rehydrates:
    // an explicit install's source is the caller's, not a prior marker's.
    reconcile_all(plugin, &desired, &scope, filter, false)
}

pub(crate) fn update_report(plugin: &Plugin, scope: Scope, source: Source) -> Result<AgentReport> {
    let _lock = lock::acquire()?;
    // `source` is the caller's `DEFAULT_SOURCE`; `reconcile_all` rehydrates each
    // agent's OWN persisted `--path` source from its OWN marker (never one agent's
    // marker broadcast onto the rest — the per-agent-marker invariant).
    let desired = Desired { source, reenable: true };
    let report = reconcile_all(plugin, &desired, &scope, &[], true)?;
    // Only a CC change strands the running session: Claude Code loads plugin
    // contents at session start with no hot-reload, whereas config-family
    // harnesses re-read their config each session. Best-effort so a flag-write
    // failure can't flip a successful update red (the same `data_root` disk error
    // surfaces through `stamp::write` inside the fan-out).
    if report.outcome_of("claude").is_some_and(|outcome| *outcome != Outcome::NoOp) {
        let _ = crate::restart::set(plugin);
    }
    Ok(report)
}

pub(crate) fn uninstall_report(plugin: &Plugin, scope: Scope) -> Result<AgentReport> {
    let _lock = lock::acquire()?;
    data_dir_precondition(plugin)?;
    let mut report = AgentReport::new();
    for id in plugin.agents {
        report.push(id, uninstall_agent(plugin, &scope, id));
    }
    Ok(report)
}

/// One agent's uninstall slice; a failure here never aborts the fan-out. An
/// absent tool has nothing of ours to remove, but the marker is agentgear's own
/// state — cleared even for a skipped agent, so an explicit uninstall never
/// orphans one. A FAILED remove keeps its marker (the install is still live;
/// the next uninstall or self_heal picks it back up).
fn uninstall_agent(plugin: &Plugin, scope: &Scope, id: &'static str) -> AgentStatus {
    let backend = match resolve(id) {
        Ok(backend) => backend,
        Err(e) => return AgentStatus::Failed(e.to_string()),
    };
    let status = if !backend.detect() {
        AgentStatus::Skipped(SkipReason::NotDetected)
    } else if !backend.capabilities().scopes.contains(&scope.as_cli()) {
        AgentStatus::Skipped(SkipReason::ScopeUnsupported)
    } else {
        match backend.remove(plugin, scope) {
            Ok(outcome) => AgentStatus::Converged(outcome),
            Err(e) => return AgentStatus::Failed(e.to_string()),
        }
    };
    match stamp::clear(plugin, scope, id) {
        Ok(()) => status,
        Err(e) => AgentStatus::Failed(e.to_string()),
    }
}

/// One reconcile pass over the configured agents. `rehydrate`: when true
/// (`update`), each agent's source is first resolved against ITS OWN stamp marker
/// (`stamp::resolve_source`), falling back to `desired.source` only absent a
/// persisted `--path`. When false (`install`), every agent converges on
/// `desired.source` exactly as given — an explicit install/`install_into` source
/// is the caller's, never a prior marker's.
fn reconcile_all(plugin: &Plugin, desired: &Desired, scope: &Scope, filter: &[&str], rehydrate: bool) -> Result<AgentReport> {
    data_dir_precondition(plugin)?;
    let mut report = AgentReport::new();
    for id in plugin.agents {
        if !filter.is_empty() && !filter.contains(id) {
            continue; // not asked for: no entry, exactly as before the report existed
        }
        report.push(id, reconcile_agent(plugin, desired, scope, id, rehydrate));
    }
    Ok(report)
}

/// One agent's reconcile slice; a failure here becomes its `Failed` entry and
/// never aborts the fan-out (one bad agent must not strand the other 24
/// mid-write). Markers stay strictly per agent: only THIS agent's marker is
/// written, and only after its own reconcile succeeded.
fn reconcile_agent(plugin: &Plugin, desired: &Desired, scope: &Scope, id: &'static str, rehydrate: bool) -> AgentStatus {
    let backend = match resolve(id) {
        Ok(backend) => backend,
        Err(e) => return AgentStatus::Failed(e.to_string()),
    };
    if !backend.detect() {
        // never forge config for a tool that isn't installed (design §0)
        return AgentStatus::Skipped(SkipReason::NotDetected);
    }
    // No surface at this scope (e.g. a repo-config-only IDE backend at user
    // scope): skip, no marker — visible in the report, but still exempt from
    // an explicit `install_into` filter's expectations, matching detect.
    if !backend.capabilities().scopes.contains(&scope.as_cli()) {
        return AgentStatus::Skipped(SkipReason::ScopeUnsupported);
    }
    let source = if rehydrate { stamp::resolve_source(plugin, scope, id, desired.source.clone()) } else { desired.source.clone() };
    // A github source has no local tree for a config-merge backend to render
    // from; only a plugin-native backend (claude, copilot-cli) can hand the ref
    // to its own CLI. A visible skip — never a mid-fan-out error after siblings
    // already wrote their configs.
    if matches!(source, Source::GitHub { .. }) && !backend.capabilities().plugins {
        return AgentStatus::Skipped(SkipReason::SourceUnsupported);
    }
    let per_agent = Desired { source, reenable: desired.reenable };
    let written = backend.reconcile(plugin, &per_agent, scope).and_then(|outcome| {
        stamp::write(plugin, scope, &per_agent.source, id)?;
        Ok(outcome)
    });
    match written {
        Ok(outcome) => AgentStatus::Converged(outcome),
        Err(e) => AgentStatus::Failed(e.to_string()),
    }
}

/// A missing data root (`HOME`/`XDG_DATA_HOME` both unset) fails every agent's
/// marker write identically, so it aborts the whole call like the lock — a
/// genuine precondition, not 25 copies of the same per-agent failure.
pub(crate) fn data_dir_precondition(plugin: &Plugin) -> Result<()> {
    crate::host::data_root(plugin).map(|_| ())
}

pub(crate) fn resolve(id: &str) -> Result<Box<dyn crate::agents::AgentBackend>> {
    backend_for(id).ok_or_else(|| Error::Tree(format!("no backend for agent `{id}` (enable its cargo feature)")))
}
