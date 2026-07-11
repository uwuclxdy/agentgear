//! Per-`(plugin, scope, project)` stamp marker. Its presence is how self_heal
//! tells "an install this crate made" from "a plugin someone else put there";
//! only `install`/`update` write it, `uninstall` and the clean-uninstall row
//! clear it. Keyed by hash so project-scope installs in different dirs never
//! shadow each other.

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
}

fn source_mode(source: &Source) -> &'static str {
    match source {
        Source::Embedded => "embedded",
        Source::GitHub { .. } => "github",
        Source::Path(_) => "path",
    }
}

fn marker_path(plugin: &Plugin, scope: &Scope) -> Result<PathBuf> {
    let root = data_root(plugin)?;
    let mut hasher = Sha256::new();
    hasher.update(plugin.name.as_bytes());
    hasher.update([0]);
    hasher.update(scope.key().as_bytes());
    Ok(root.join("markers").join(hex(&hasher.finalize())))
}

/// A corrupt or partially-written marker is treated as absent, not an error:
/// self_heal then re-derives the true state from `list --json` and rewrites it.
pub(crate) fn read(plugin: &Plugin, scope: &Scope) -> Result<Option<Marker>> {
    let path = marker_path(plugin, scope)?;
    match fs::read(&path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io { context: format!("reading marker {}", path.display()), source: e }),
    }
}

pub(crate) fn write(plugin: &Plugin, scope: &Scope, source: &Source) -> Result<()> {
    let path = marker_path(plugin, scope)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).io_ctx(|| format!("creating {}", parent.display()))?;
    }
    let marker = Marker {
        binary_version: plugin.version.to_string(),
        plugin_version: plugin.version.to_string(),
        source_mode: source_mode(source).to_string(),
        scope: scope.as_cli().to_string(),
        project_path: scope.cwd().map(|p| p.display().to_string()),
    };
    let bytes = serde_json::to_vec_pretty(&marker).map_err(|source| Error::Json { what: "stamp marker".into(), source })?;
    fs::write(&path, bytes).io_ctx(|| format!("writing marker {}", path.display()))
}

pub(crate) fn clear(plugin: &Plugin, scope: &Scope) -> Result<()> {
    let path = marker_path(plugin, scope)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io { context: format!("clearing marker {}", path.display()), source: e }),
    }
}
