//! The SessionStart entrypoint. The hook is part of the plugin, so it only ever
//! fires on an install that already exists: self_heal repairs *broken* installs,
//! never resurrects an *absent* one (a resurrected uninstall reads as adware).
//!
//! Driven by the stamp marker × `list --json` state (design §6):
//!
//! | marker | plugin | action |
//! |--------|--------|--------|
//! | absent | absent  | no-op |
//! | absent | present | adopt (converge, then write marker) |
//! | present| absent  | clear marker, no-op (never resurrect) |
//! | present| present, disabled | no-op (never re-enable) |
//! | present| present, healthy + monotonic-current | no-op |
//! | present| present, broken/stale | repair/update, write marker |
//!
//! v1 heals the Claude backend (the only one). It does one `plugin list --json`
//! read for the never-resurrect gate + healthy fast path; convergence (which
//! re-reads) only runs on a genuinely broken/stale install.

use std::path::Path;

use crate::agents::claude;
use crate::cli::{ClaudeCli, version_lt};
use crate::error::Result;
use crate::host::{Desired, Outcome, Plugin, Scope, Source};
use crate::{lock, stamp};

pub(crate) fn self_heal(plugin: &Plugin, source: Source) -> Result<Outcome> {
    // The SessionStart hook targets user-scoped installs; project scope has no
    // stable session context to key on in v1.
    let scope = Scope::User;
    let _lock = lock::acquire()?;

    let marker = stamp::read(plugin, &scope)?;
    let cli = ClaudeCli::locate()?;
    let entry = claude::find_plugin(&cli, &scope, plugin.name, plugin.marketplace)?;

    match (marker.is_some(), entry) {
        (false, None) => Ok(Outcome::NoOp),

        (false, Some(_present)) => {
            // Adopt: converge (repairs it if broken), then record ownership.
            let outcome = claude::reconcile(plugin, &Desired { source, reenable: false }, &scope)?;
            stamp::write(plugin, &scope, source)?;
            Ok(match outcome {
                Outcome::NoOp => Outcome::Adopted,
                other => other,
            })
        }

        (true, None) => {
            // Clean uninstall under our marker: forget it, do not reinstall.
            stamp::clear(plugin, &scope)?;
            Ok(Outcome::Cleared)
        }

        (true, Some(entry)) => {
            if entry.enabled == Some(false) {
                return Ok(Outcome::NoOp); // never re-enable a deliberate disable
            }
            let files_ok = entry.install_path.as_ref().is_none_or(|p| Path::new(p).exists());
            let monotonic_current = !version_lt(entry.version.as_deref(), plugin.version);
            if files_ok && monotonic_current {
                return Ok(Outcome::NoOp); // healthy fast path: no mutation, no downgrade
            }
            let outcome = claude::reconcile(plugin, &Desired { source, reenable: false }, &scope)?;
            stamp::write(plugin, &scope, source)?;
            Ok(outcome)
        }
    }
}
