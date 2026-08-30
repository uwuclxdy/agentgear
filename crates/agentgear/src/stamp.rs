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
    /// The harness's own status-line value from before this crate wrote the host's
    /// one, kept as raw JSON so any harness's shape round-trips losslessly. Never
    /// written anymore — the automatic slot wiring is retired — but markers written
    /// by pre-retirement binaries still carry it, and [`statusline::user_original`]
    /// still reads it so a host's print subcommand keeps composing the user's row.
    /// Carried forward by every later [`write`] for the same reason: the marker is
    /// rebuilt from scratch on each install/update/self_heal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statusline_original: Option<serde_json::Value>,
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
    /// Set while a backend is between the two halves of a reinstall (the only
    /// sequence that re-copies a tree into a harness at an unchanged version), and
    /// cleared by the pass that completes one.
    ///
    /// It is what separates "the user uninstalled this" from "we uninstalled it and
    /// never got it back": both leave a marker beside an absent plugin, and self_heal
    /// forgets the first on sight. A `plugin install` that fails, or a SessionStart
    /// hook killed between the calls, would otherwise end as a permanent uninstall
    /// that every later session reads as the user's own choice.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reinstalling: bool,
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
        tree_hash: None,
        reinstalling: false,
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
    // Every install/update/self_heal rebuilds the marker, so fields recorded by a
    // reconcile have to be carried across explicitly. The tree hash is that shape: the
    // reconcile that converged the harness records it, and dropping it here would make
    // every pass re-converge a tree the harness already holds. A legacy
    // `statusline_original` stash rides along too, or the first rebuild would erase the
    // only copy of a pre-retirement user's row that `statusline::user_original` still
    // composes with. `reinstalling` is deliberately NOT carried: this runs only after
    // a backend's whole reconcile succeeded, which is exactly the state that ends one.
    let previous = read(plugin, scope, agent)?;
    marker.statusline_original = previous.as_ref().and_then(|m| m.statusline_original.clone());
    marker.tree_hash = previous.and_then(|m| m.tree_hash);
    write_marker(&path, &marker)
}

/// Mark this agent as being between the two halves of a reinstall, BEFORE the uninstall
/// runs. Its own write has to land first: what it defends against is the process never
/// reaching the second half.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
pub(crate) fn begin_reinstall(plugin: &Plugin, scope: &Scope, source: &Source, agent: &str) -> Result<()> {
    amend(plugin, scope, source, agent, |marker| marker.reinstalling = true)
}

/// Record what this agent's harness now holds, after the CLI call that handed the tree
/// over returned: the tree hash where there is a local tree, and in every case the end
/// of a reinstall. Recording either before that would claim a convergence a failed
/// install never made, and the next pass would skip the repair.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
pub(crate) fn record_converged(plugin: &Plugin, scope: &Scope, source: &Source, agent: &str, tree_hash: Option<&str>) -> Result<()> {
    amend(plugin, scope, source, agent, |marker| {
        if let Some(hash) = tree_hash {
            marker.tree_hash = Some(hash.to_string());
        }
        marker.reinstalling = false;
    })
}

/// Read-modify-write one field of this agent's marker, creating it when the reconcile
/// that is amending it has not been stamped yet (the fan-out stamps only after a
/// backend's whole reconcile succeeds).
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn amend(plugin: &Plugin, scope: &Scope, source: &Source, agent: &str, edit: impl FnOnce(&mut Marker)) -> Result<()> {
    let path = marker_path(plugin, scope, agent)?;
    let mut marker = read(plugin, scope, agent)?.unwrap_or_else(|| base_marker(plugin, scope, source, agent));
    edit(&mut marker);
    write_marker(&path, &marker)
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
