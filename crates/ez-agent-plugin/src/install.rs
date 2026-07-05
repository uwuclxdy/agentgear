//! Lifecycle orchestration: acquire the shared lock, drive each configured agent
//! backend to converge, then stamp (or clear) the marker. The lock is held across
//! the whole sequence so two tools self-healing at once serialize.

use crate::agents::backend_for;
use crate::error::{Error, Result};
use crate::host::{Desired, Outcome, Plugin, Scope, Source};
use crate::{lock, stamp};

pub(crate) fn install(plugin: &Plugin, scope: Scope, source: Source) -> Result<Outcome> {
    let _lock = lock::acquire()?;
    let outcome = reconcile_all(plugin, &Desired { source, reenable: true }, &scope)?;
    stamp::write(plugin, &scope, source)?;
    Ok(outcome)
}

pub(crate) fn update(plugin: &Plugin, scope: Scope, source: Source) -> Result<Outcome> {
    let _lock = lock::acquire()?;
    let outcome = reconcile_all(plugin, &Desired { source, reenable: true }, &scope)?;
    stamp::write(plugin, &scope, source)?;
    Ok(outcome)
}

pub(crate) fn uninstall(plugin: &Plugin, scope: Scope) -> Result<Outcome> {
    let _lock = lock::acquire()?;
    let mut outcome = Outcome::NoOp;
    for id in plugin.agents {
        let backend = resolve(id)?;
        outcome = merge(outcome, backend.remove(plugin, &scope)?);
    }
    stamp::clear(plugin, &scope)?;
    Ok(outcome)
}

fn reconcile_all(plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
    let mut outcome = Outcome::NoOp;
    for id in plugin.agents {
        let backend = resolve(id)?;
        outcome = merge(outcome, backend.reconcile(plugin, desired, scope)?);
    }
    Ok(outcome)
}

fn resolve(id: &str) -> Result<Box<dyn crate::agents::AgentBackend>> {
    backend_for(id).ok_or_else(|| Error::Tree(format!("no backend for agent `{id}` (enable its cargo feature)")))
}

/// Across the configured agents, surface the first real change (a change outranks
/// a no-op). v1 ships one backend, so this is effectively that backend's outcome.
fn merge(acc: Outcome, next: Outcome) -> Outcome {
    if acc == Outcome::NoOp { next } else { acc }
}
