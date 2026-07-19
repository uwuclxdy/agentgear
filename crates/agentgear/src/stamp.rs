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

pub(crate) fn write(plugin: &Plugin, scope: &Scope, source: &Source, agent: &str) -> Result<()> {
    let path = marker_path(plugin, scope, agent)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).io_ctx(|| format!("creating {}", parent.display()))?;
    }
    let marker = Marker {
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
    };
    let bytes = serde_json::to_vec_pretty(&marker).map_err(|source| Error::Json { what: "stamp marker".into(), source })?;
    fs::write(&path, bytes).io_ctx(|| format!("writing marker {}", path.display()))
}

pub(crate) fn clear(plugin: &Plugin, scope: &Scope, agent: &str) -> Result<()> {
    let path = marker_path(plugin, scope, agent)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io { context: format!("clearing marker {}", path.display()), source: e }),
    }
}

/// The pure half of source resolution: derive `Source::Path` from an
/// already-read marker, else fall back to `default`. Split out of
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
    // Only "path" rehydrates. Ceiling: an explicit `install(Source::Embedded)` on
    // a `default_source = "github"` host stamps "embedded" but resolves back to the
    // github default here, so update/self_heal/uninstall treat that agent as
    // github-sourced. The github gate then skips a config backend on every heal
    // pass, and on uninstall the same skip still clears the marker while the
    // config writes stay on disk, orphaning them with nothing left to reclaim them.
    // Upgrade path: rehydrate `source_mode == "embedded"` to `Source::Embedded`
    // once it is decided whether an explicit embedded install should pin embedded
    // over the host's compile-time default.
    match marker {
        Some(m) if m.source_mode == "path" => m.source_path.clone().map(|p| Source::Path(PathBuf::from(p))).unwrap_or(default),
        _ => default,
    }
}

/// Rehydrate `agent`'s own persisted `Source::Path` for `update`/`doctor`, neither
/// of which carry a runtime `Source` of their own (only the compile-time
/// `DEFAULT_SOURCE`, which the derive only ever emits as `embedded`/`github`).
/// `Embedded`/`GitHub` already resolve correctly from `DEFAULT_SOURCE`, so this
/// only ever overrides toward a path. Reads `agent`'s marker fresh; self_heal's
/// `heal_agent` already has it and calls [`source_from_marker`] directly instead.
pub(crate) fn resolve_source(plugin: &Plugin, scope: &Scope, agent: &str, default: Source) -> Source {
    let marker = read(plugin, scope, agent).ok().flatten();
    source_from_marker(marker.as_ref(), default)
}

#[cfg(test)]
#[path = "../tests/unit/stamp.rs"]
mod stamp_tests;
