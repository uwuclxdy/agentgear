//! Shared reader for Claude Code's own on-disk plugin registry
//! (`<home>/.claude/plugins/installed_plugins.json`). Every backend that detects
//! "does CC's native plugin-tree ingestion already cover this plugin" reads the
//! same file the same way: omp's `claude-plugins` provider and cursor's
//! default-on `loadClaude` loader both resolve it HOME-based (`join(HOME,
//! ".claude")`, hardcoded — neither honors `CLAUDE_CONFIG_DIR`), so a relocated
//! config dir moves the registry off this path for every caller, and each reads
//! not-listed (translate, never lose coverage) rather than mis-covering.

use std::fs;
use std::path::Path;

use serde_json::Value;

/// True when CC's `installed_plugins.json` lists `id` with at least one install
/// entry. Mirrors omp's `parseClaudePluginsRegistry`: a numeric top-level
/// `version` key is mandatory — without it the whole registry reads as absent,
/// matching every native consumer of this file. The read is HOME-based, matching
/// each caller's own path resolution, so a relocated `CLAUDE_CONFIG_DIR` reads as
/// not-listed.
pub(crate) fn registry_lists_plugin(path: &Path, id: &str) -> bool {
    let Some(root) = fs::read(path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
        return false;
    };
    root.get("version").is_some_and(Value::is_number)
        && root
            .get("plugins")
            .and_then(Value::as_object)
            .and_then(|m| m.get(id))
            .and_then(Value::as_array)
            .is_some_and(|entries| !entries.is_empty())
}

#[cfg(test)]
#[path = "../../tests/unit/ccregistry.rs"]
mod ccregistry_tests;
