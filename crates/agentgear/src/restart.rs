//! Restart-pending flag: a presence-only file at `<data_root>/restart-pending`
//! marking that an update landed which the running Claude Code session has not
//! loaded yet. CC reads plugin hooks/commands at session start with no mid-session
//! hot-reload, so an out-of-band `setup update` leaves the running session stale
//! until the user restarts.
//!
//! `update` and every `self_heal` reconcile set it, and only when that reconcile
//! actually changed the on-disk plugin (a takeover of an unowned install included, since
//! the session is stranded by the tree moving under it rather than by whose install it
//! was); a reconcile that changed nothing clears it, as does a heal that found the
//! install already current — a fresh session loaded what CC holds, so the nag no longer
//! applies. `install` never sets it (a first install precedes any session that relies on
//! the plugin).
//!
//! The flag is advisory UX state, not transactional: a write/clear failure is
//! swallowed (`let _ = …`) so it can never flip a successful lifecycle op red or
//! turn a healthy self_heal into a failure. A real disk failure surfaces anyway —
//! `stamp::write` and `materialize` hit the same `data_root` and do propagate.

use std::fs;
use std::path::PathBuf;

use crate::error::{Error, IoContext, Result};
use crate::host::{Plugin, data_root};

const FLAG: &str = "restart-pending";

fn path(plugin: &Plugin) -> Result<PathBuf> {
    Ok(data_root(plugin)?.join(FLAG))
}

/// Mark that an update landed. Idempotent. `data_root` already exists by the time
/// a mutate runs (`materialize` created it); the `create_dir_all` keeps `set`
/// self-sufficient for any future caller that has not materialized.
pub(crate) fn set(plugin: &Plugin) -> Result<()> {
    let path = path(plugin)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).io_ctx(|| format!("creating {}", parent.display()))?;
    }
    fs::write(&path, []).io_ctx(|| format!("writing {}", path.display()))
}

/// Forget a pending update. A missing file is success (already cleared / never set).
pub(crate) fn clear(plugin: &Plugin) -> Result<()> {
    match fs::remove_file(path(plugin)?) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io { context: "clearing restart-pending".into(), source }),
    }
}

/// Whether an update is pending. A missing or unreadable flag reads as "no"
/// (`stamp.rs` leniency): a transient read failure must not turn the per-prompt
/// `UserPromptSubmit` hook into a permanent nag.
pub(crate) fn pending(plugin: &Plugin) -> Result<Option<()>> {
    match fs::metadata(path(plugin)?) {
        Ok(_) => Ok(Some(())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io { context: "reading restart-pending".into(), source }),
    }
}

/// The message a `check-restart` subcommand feeds back to the model when an update
/// is pending. Pure so the hook path is just `pending` + this string.
///
/// Phrased as factual environment state, never an imperative: CC's docs warn that
/// text framed as a system command ("restart now", "ask the user to…") trips the
/// prompt-injection defenses, so the model surfaces it to the user instead of
/// treating it as context. Stating the situation lets the model relay it naturally.
/// `/reload-plugins` is named first because it applies the update without losing
/// the session; a full restart is the fallback.
pub(crate) fn message(name: &str, version: &str) -> String {
    format!(
        "The `{name}` Claude Code plugin was updated to {version} after this session started. \
         This session still has the previous version loaded; the updated hooks, commands, and \
         agents take effect after a `/reload-plugins`, or a full Claude Code restart."
    )
}

#[cfg(test)]
#[path = "../tests/unit/restart.rs"]
mod restart_tests;
