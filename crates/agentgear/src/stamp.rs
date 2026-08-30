//! Per-`(plugin, scope, project, agent)` stamp marker. Its presence is how
//! self_heal tells "an install this crate made" from "a plugin someone else put
//! there"; only `install`/`update` write it, `uninstall` and the clean-uninstall
//! row clear it. Keyed by hash so installs in different project dirs — or into
//! different agent backends — never shadow each other.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, IoContext, Result};
use crate::host::{Plugin, Scope, Source, data_root};
use crate::util::hex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Marker {
    pub binary_version: String,
    pub plugin_version: String,
    pub source_mode: String,
    pub scope: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    /// Set only for `source_mode == "path"`. `update`/`self_heal`/`doctor` have no
    /// runtime `Source` of their own (only the compile-time `DEFAULT_SOURCE`), so
    /// this is what lets a `--path` install stay path-only through repair instead
    /// of drifting to the baked blob or a GitHub ref. `#[serde(default, ...)]`
    /// keeps a marker written before this field existed loading fine (deserializes
    /// to `None`, same as an embedded/github marker written today) — but that also
    /// means a `--path` install stamped by a pre-fix binary carries
    /// `source_mode == "path"` with `source_path: None` forever; it keeps drifting
    /// toward `DEFAULT_SOURCE` on repair until a `setup --path` re-run refreshes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// The harness's own status-line value from before we overwrote it, kept as
    /// raw JSON so any harness's shape round-trips losslessly. Absent when the slot
    /// was empty at install time — `remove` then deletes the key outright instead of
    /// restoring anything. Written by [`stash_statusline`] and carried forward by
    /// every later [`write`]: the marker is rebuilt from scratch on each
    /// install/update/self_heal, so without that carry an `update` between install
    /// and uninstall would erase the user's original.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statusline_original: Option<serde_json::Value>,
    /// The command string this agent's backend last wrote into the harness's slot,
    /// `${AGENTGEAR_CLIENT}` already expanded. Ownership of the slot is otherwise
    /// decided against the command the host declares *now*, so a release that renames
    /// its own status-line subcommand would read its own previous value as foreign and
    /// stash it over the user's real original. This is the record that keeps that
    /// value ours across the rename.
    ///
    /// Absent for any install predating the field, which puts ownership back on the
    /// current-command compare alone — the pre-existing behavior, rename hazard
    /// included. Carried forward by every later [`write`] for the same reason
    /// [`Self::statusline_original`] is: the marker is rebuilt from scratch on each
    /// install/update/self_heal, and `reconcile` records this before that rebuild.
    ///
    /// Written by [`record_statusline_command`], and only once the settings write it
    /// describes has actually landed — a refused write (an unparseable settings file)
    /// must leave the previous record standing, because that one is still true of
    /// what is in the slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statusline_command: Option<String>,
    /// The materialized tree hash (`materialize::content_hash`) this agent's harness
    /// was last handed, written only once the CLI call that handed it over succeeded.
    /// A plugin-native backend copies the tree into its own cache keyed on the plugin
    /// VERSION, so a same-version tree edit is invisible to every version comparison
    /// the registry offers; this is the record that makes it visible.
    ///
    /// Absent for a github source (no local tree) and for any install predating the
    /// field, both of which read as "unknown content" — the first ignores the gate,
    /// the second converges once and records what it handed over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
}

fn source_mode(source: &Source) -> &'static str {
    match source {
        Source::Embedded => "embedded",
        Source::GitHub { .. } => "github",
        Source::Path(_) => "path",
    }
}

fn marker_path(plugin: &Plugin, scope: &Scope, agent: &str) -> Result<PathBuf> {
    let root = data_root(plugin)?;
    let mut hasher = Sha256::new();
    hasher.update(plugin.name.as_bytes());
    hasher.update([0]);
    hasher.update(scope.key().as_bytes());
    hasher.update([0]);
    hasher.update(agent.as_bytes());
    Ok(root.join("markers").join(hex(&hasher.finalize())))
}

/// A corrupt or partially-written marker is treated as absent, not an error:
/// self_heal then re-derives the true state from the backend's `probe` and
/// rewrites it.
pub(crate) fn read(plugin: &Plugin, scope: &Scope, agent: &str) -> Result<Option<Marker>> {
    let path = marker_path(plugin, scope, agent)?;
    match fs::read(&path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io { context: format!("reading marker {}", path.display()), source: e }),
    }
}

fn base_marker(plugin: &Plugin, scope: &Scope, source: &Source, agent: &str) -> Marker {
    Marker {
        binary_version: plugin.version.to_string(),
        plugin_version: plugin.version.to_string(),
        source_mode: source_mode(source).to_string(),
        scope: scope.as_cli().to_string(),
        agent: agent.to_string(),
        project_path: scope.cwd().map(|p| p.display().to_string()),
        // WHY `display()`: matches `project_path`'s pre-existing lossy-on-non-UTF-8
        // approach (a `PathBuf` doesn't round-trip through JSON otherwise); it is
        // now load-bearing (rehydrated back into a real `Source::Path` for
        // materialize), so a non-UTF-8 path root would rehydrate with U+FFFD
        // replacement bytes. A lossless encoding is a follow-up if that ever bites.
        source_path: match source {
            Source::Path(p) => Some(p.display().to_string()),
            Source::Embedded | Source::GitHub { .. } => None,
        },
        statusline_original: None,
        statusline_command: None,
        tree_hash: None,
    }
}

fn write_marker(path: &std::path::Path, marker: &Marker) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).io_ctx(|| format!("creating {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(marker).map_err(|source| Error::Json { what: "stamp marker".into(), source })?;
    fs::write(path, bytes).io_ctx(|| format!("writing marker {}", path.display()))
}

pub(crate) fn write(plugin: &Plugin, scope: &Scope, source: &Source, agent: &str) -> Result<()> {
    let path = marker_path(plugin, scope, agent)?;
    let mut marker = base_marker(plugin, scope, source, agent);
    // Every install/update/self_heal rebuilds the marker, so the fields a reconcile
    // records have to be carried across explicitly. Without the stash carry the user's
    // pre-existing status line is lost on the first re-write and uninstall has nothing
    // to restore; without the command carry the record `reconcile` just wrote is erased
    // by the `write` that follows it in the very same pass, and the next release's
    // rename reads our own value as foreign again. The tree hash is the same shape: the
    // reconcile that converged the harness records it, and dropping it here would make
    // every pass re-converge a tree the harness already holds.
    let previous = read(plugin, scope, agent)?;
    marker.statusline_original = previous.as_ref().and_then(|m| m.statusline_original.clone());
    marker.statusline_command = previous.as_ref().and_then(|m| m.statusline_command.clone());
    marker.tree_hash = previous.and_then(|m| m.tree_hash);
    write_marker(&path, &marker)
}

crate::agents::cfg_statusline_backends! {
/// Record `original` as the status-line value that was in the harness's settings
/// before this agent's backend wrote the host's own.
///
/// Unconditional: whatever it is handed replaces any earlier stash, so NOT calling it
/// is the only thing that preserves one. That is why `reconcile` settles ownership
/// before it gets here and skips it entirely for an empty slot or a value already ours
/// — an empty slot must not erase what the user had before we ever wrote (self_heal
/// re-adding a line they deleted), and a value of ours is not theirs to record.
pub(crate) fn stash_statusline(plugin: &Plugin, scope: &Scope, source: &Source, agent: &str, original: serde_json::Value) -> Result<()> {
    amend(plugin, scope, source, agent, |marker| marker.statusline_original = Some(original))
}
}

crate::agents::cfg_statusline_backends! {
/// Record `command` as the command string this agent's backend just wrote into the
/// harness's status-line slot, so a later release that renames its own status-line
/// subcommand still recognises the value as ours.
pub(crate) fn record_statusline_command(plugin: &Plugin, scope: &Scope, source: &Source, agent: &str, command: &str) -> Result<()> {
    amend(plugin, scope, source, agent, |marker| marker.statusline_command = Some(command.to_string()))
}
}

/// Record the materialized tree hash this agent's harness now holds, after the CLI
/// call that handed the tree over returned. Recording it before that would claim a
/// convergence a failed install never made, and the next pass would skip the repair.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
pub(crate) fn record_tree_hash(plugin: &Plugin, scope: &Scope, source: &Source, agent: &str, tree_hash: &str) -> Result<()> {
    amend(plugin, scope, source, agent, |marker| marker.tree_hash = Some(tree_hash.to_string()))
}

crate::agents::cfg_statusline_backends! {
/// Read-modify-write one field of this agent's marker, creating it when the reconcile
/// that is amending it has not been stamped yet (the fan-out stamps only after a
/// backend's whole reconcile succeeds).
fn amend(plugin: &Plugin, scope: &Scope, source: &Source, agent: &str, edit: impl FnOnce(&mut Marker)) -> Result<()> {
    let path = marker_path(plugin, scope, agent)?;
    let mut marker = read(plugin, scope, agent)?.unwrap_or_else(|| base_marker(plugin, scope, source, agent));
    edit(&mut marker);
    write_marker(&path, &marker)
}
}

pub(crate) fn clear(plugin: &Plugin, scope: &Scope, agent: &str) -> Result<()> {
    let path = marker_path(plugin, scope, agent)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io { context: format!("clearing marker {}", path.display()), source: e }),
    }
}

/// The pure half of source resolution: derive the marker's own persisted
/// source, else fall back to `default`. Split out of
/// [`resolve_source`] so a caller that already has the marker in hand
/// (self_heal's `heal_agent`, which reads it for its own marker-presence check)
/// does not have to read it a second time.
///
/// Per-agent by construction — the caller supplies one agent's own marker, never
/// one scanned across `plugin.agents`. Every agent is stamped with the source it
/// was individually converged with (`install_into`/`setup --agent` lets one agent
/// diverge from the rest), so broadcasting any single marker's source onto every
/// agent would let one backend's `--path` install silently re-point a sibling
/// installed from `Embedded`/`GitHub` (or vice versa) on the next unfiltered
/// `update`/`self_heal`/`doctor` — the exact per-agent-marker invariant
/// `docs/design.md` calls out (installing into `[claude, codex]` must never
/// confuse one backend's marker for another's).
pub(crate) fn source_from_marker(marker: Option<&Marker>, default: Source) -> Source {
    // "path" and "embedded" markers each rehydrate the source they were stamped
    // with, so an explicit `install(Source::Embedded)` on a `default_source =
    // "github"` host stays embedded through update/self_heal/uninstall (the
    // github gate would otherwise skip that config backend on every heal pass and
    // orphan its writes on uninstall). The fallthrough covers a genuinely absent
    // or unknown marker, plus a "path" marker with no persisted path. A "github"
    // marker also falls through: it persists no repo or ref, so github is only ever
    // rebuilt from `default`. On a non-github-default host that lossily un-pins an
    // explicit github install to the compile-time default (accepted; v1 has no
    // consumer pairing an explicit github install with a non-github default).
    match marker {
        Some(m) if m.source_mode == "path" => m.source_path.clone().map(|p| Source::Path(PathBuf::from(p))).unwrap_or(default),
        Some(m) if m.source_mode == "embedded" => Source::Embedded,
        _ => default,
    }
}

/// Rehydrate `agent`'s own persisted source (`Path` or an explicit `Embedded`)
/// for `update`/`doctor`, neither of which carry a runtime `Source` of their own
/// (only the compile-time `DEFAULT_SOURCE`, which the derive only ever emits as
/// `embedded`/`github`). Reads `agent`'s marker fresh; self_heal's
/// `heal_agent` already has it and calls [`source_from_marker`] directly instead.
pub(crate) fn resolve_source(plugin: &Plugin, scope: &Scope, agent: &str, default: Source) -> Source {
    let marker = read(plugin, scope, agent).ok().flatten();
    source_from_marker(marker.as_ref(), default)
}

#[cfg(test)]
#[path = "../tests/unit/stamp.rs"]
mod stamp_tests;
