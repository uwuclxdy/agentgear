//! Lifecycle orchestration: acquire the shared lock, fan out over the plugin's
//! configured agent backends — converging (or removing) each and stamping a
//! per-agent marker — then merge their outcomes. The lock is held across the whole
//! sequence so two tools self-healing at once serialize.

use crate::agents::backend_for;
use crate::error::{Error, Result};
use crate::host::{Desired, Outcome, Plugin, Scope, Source};
use crate::{lock, stamp};

pub(crate) fn install(plugin: &Plugin, scope: Scope, source: Source) -> Result<Outcome> {
    install_filtered(plugin, scope, source, &[])
}

/// Install into the subset of `plugin.agents` whose id is in `filter` (an empty
/// filter = all). Powers `PluginHost::install_into` / `setup --agent <id>`.
pub(crate) fn install_filtered(plugin: &Plugin, scope: Scope, source: Source, filter: &[&str]) -> Result<Outcome> {
    let _lock = lock::acquire()?;
    let desired = Desired { source, reenable: true };
    // A first install never sets restart-pending: it precedes any session that
    // relies on the plugin, so there is nothing stale to reload.
    Ok(reconcile_all(plugin, &desired, &scope, filter)?.merged)
}

pub(crate) fn update(plugin: &Plugin, scope: Scope, source: Source) -> Result<Outcome> {
    let _lock = lock::acquire()?;
    // `source` here is the caller's `DEFAULT_SOURCE`; a prior `--path` install left
    // a marker recording the real runtime path, which `resolve_source` prefers.
    let source = stamp::resolve_source(plugin, &scope, source);
    let desired = Desired { source, reenable: true };
    let fan = reconcile_all(plugin, &desired, &scope, &[])?;
    // Only a CC change strands the running session: Claude Code loads plugin
    // contents at session start with no hot-reload, whereas config-family
    // harnesses re-read their config each session. Best-effort so a flag-write
    // failure can't flip a successful update red (the same `data_root` disk error
    // surfaces through `stamp::write` inside the fan-out).
    if fan.claude != Outcome::NoOp {
        let _ = crate::restart::set(plugin);
    }
    Ok(fan.merged)
}

pub(crate) fn uninstall(plugin: &Plugin, scope: Scope) -> Result<Outcome> {
    let _lock = lock::acquire()?;
    let mut outcome = Outcome::NoOp;
    for id in plugin.agents {
        let backend = resolve(id)?;
        // An absent tool has nothing of ours to remove, but the marker is agentgear's
        // own state — clear it unconditionally so an explicit uninstall never orphans one.
        if backend.detect() && backend.capabilities().scopes.contains(&scope.as_cli()) {
            outcome = merge(outcome, backend.remove(plugin, &scope)?);
        }
        stamp::clear(plugin, &scope, id)?;
    }
    Ok(outcome)
}

/// One reconcile pass's result: the merged outcome plus the Claude backend's own
/// outcome (which alone gates the CC-only restart flag).
struct FanOut {
    merged: Outcome,
    claude: Outcome,
}

fn reconcile_all(plugin: &Plugin, desired: &Desired, scope: &Scope, filter: &[&str]) -> Result<FanOut> {
    let mut merged = Outcome::NoOp;
    let mut claude = Outcome::NoOp;
    for id in plugin.agents {
        if !filter.is_empty() && !filter.contains(id) {
            continue;
        }
        let backend = resolve(id)?;
        if !backend.detect() {
            continue; // never forge config for a tool that isn't installed (design §0)
        }
        // No surface at this scope (e.g. a repo-config-only IDE backend at user
        // scope): skip, no marker. Deliberately silent even for an explicit
        // `install_into` filter, matching the detect-skip semantic above.
        if !backend.capabilities().scopes.contains(&scope.as_cli()) {
            continue;
        }
        let outcome = backend.reconcile(plugin, desired, scope)?;
        stamp::write(plugin, scope, &desired.source, id)?;
        if *id == "claude" {
            claude = outcome.clone();
        }
        merged = merge(merged, outcome);
    }
    Ok(FanOut { merged, claude })
}

pub(crate) fn resolve(id: &str) -> Result<Box<dyn crate::agents::AgentBackend>> {
    backend_for(id).ok_or_else(|| Error::Tree(format!("no backend for agent `{id}` (enable its cargo feature)")))
}

/// Across the configured agents, surface the first real change (a change outranks
/// a no-op).
pub(crate) fn merge(acc: Outcome, next: Outcome) -> Outcome {
    if acc == Outcome::NoOp { next } else { acc }
}
