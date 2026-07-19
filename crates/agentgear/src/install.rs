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
    let mut report = AgentReport::new();
    for id in plugin.agents {
        let backend = resolve(id)?;
        // An absent tool has nothing of ours to remove, but the marker is agentgear's
        // own state — clear it unconditionally so an explicit uninstall never orphans one.
        if !backend.detect() {
            report.push(id, AgentStatus::Skipped(SkipReason::NotDetected));
        } else if !backend.capabilities().scopes.contains(&scope.as_cli()) {
            report.push(id, AgentStatus::Skipped(SkipReason::ScopeUnsupported));
        } else {
            report.push(id, AgentStatus::Converged(backend.remove(plugin, &scope)?));
        }
        stamp::clear(plugin, &scope, id)?;
    }
    Ok(report)
}

/// One reconcile pass over the configured agents. `rehydrate`: when true
/// (`update`), each agent's source is first resolved against ITS OWN stamp marker
/// (`stamp::resolve_source`), falling back to `desired.source` only absent a
/// persisted `--path`. When false (`install`), every agent converges on
/// `desired.source` exactly as given — an explicit install/`install_into` source
/// is the caller's, never a prior marker's.
fn reconcile_all(plugin: &Plugin, desired: &Desired, scope: &Scope, filter: &[&str], rehydrate: bool) -> Result<AgentReport> {
    let mut report = AgentReport::new();
    for id in plugin.agents {
        if !filter.is_empty() && !filter.contains(id) {
            continue; // not asked for: no entry, exactly as before the report existed
        }
        let backend = resolve(id)?;
        if !backend.detect() {
            // never forge config for a tool that isn't installed (design §0)
            report.push(id, AgentStatus::Skipped(SkipReason::NotDetected));
            continue;
        }
        // No surface at this scope (e.g. a repo-config-only IDE backend at user
        // scope): skip, no marker — visible in the report, but still exempt from
        // an explicit `install_into` filter's expectations, matching detect.
        if !backend.capabilities().scopes.contains(&scope.as_cli()) {
            report.push(id, AgentStatus::Skipped(SkipReason::ScopeUnsupported));
            continue;
        }
        let source = if rehydrate { stamp::resolve_source(plugin, scope, id, desired.source.clone()) } else { desired.source.clone() };
        let per_agent = Desired { source, reenable: desired.reenable };
        let outcome = backend.reconcile(plugin, &per_agent, scope)?;
        stamp::write(plugin, scope, &per_agent.source, id)?;
        report.push(id, AgentStatus::Converged(outcome));
    }
    Ok(report)
}

pub(crate) fn resolve(id: &str) -> Result<Box<dyn crate::agents::AgentBackend>> {
    backend_for(id).ok_or_else(|| Error::Tree(format!("no backend for agent `{id}` (enable its cargo feature)")))
}
