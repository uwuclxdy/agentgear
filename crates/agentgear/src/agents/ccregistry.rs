//! Shared readers for Claude Code's own on-disk registry files.
//!
//! Two call sites, two path resolutions:
//! - omp's `claude-plugins` provider and cursor's default-on `loadClaude` loader
//!   both resolve `installed_plugins.json` HOME-based (`join(HOME, ".claude")`,
//!   hardcoded — neither honors `CLAUDE_CONFIG_DIR`), so a relocated config dir
//!   moves the registry off this path for every caller, and each reads
//!   not-listed (translate, never lose coverage) rather than mis-covering.
//! - the claude backend's spawn-free heal-needed classifier reads CC's registry
//!   files (`installed_plugins.json`, `known_marketplaces.json`, `settings.json`)
//!   from the config dir CC actually uses, so the caller passes the dir in.

use std::fs;
use std::path::Path;

use serde_json::Value;

#[cfg(feature = "claude")]
use std::ffi::OsStr;
#[cfg(feature = "claude")]
use std::path::PathBuf;

#[cfg(feature = "claude")]
use crate::manifest::MarketplaceEntry;

/// True when CC's `installed_plugins.json` lists `id` with at least one install
/// entry. Mirrors omp's `parseClaudePluginsRegistry`: a numeric top-level
/// `version` key is mandatory — without it the whole registry reads as absent,
/// matching every native consumer of this file. The read is HOME-based, matching
/// each caller's own path resolution, so a relocated `CLAUDE_CONFIG_DIR` reads as
/// not-listed.
#[cfg(any(feature = "omp", feature = "cursor"))]
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

/// The config dir CC uses for its plugin registry files, or why it cannot be told.
#[cfg(feature = "claude")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CcConfigDir {
    /// A concrete dir: a non-empty `CLAUDE_CONFIG_DIR`, else `$HOME/.claude`.
    Dir(PathBuf),
    /// `CLAUDE_CONFIG_DIR` is set but empty. CC reads the empty string as the cwd
    /// (probed 2.1.220), so the registry is NOT under `$HOME/.claude` and a file
    /// reader cannot guess which dir it landed in.
    EmptyOverride,
    /// `CLAUDE_CONFIG_DIR` is unset and there is no home directory.
    NoHome,
}

/// The config dir CC uses: a non-empty `CLAUDE_CONFIG_DIR`, else `$HOME/.claude`.
/// Home comes from `dirs::home_dir()` (the crate's precedent, and the source CC's
/// libuv resolves `USERPROFILE` from on Windows), not a raw `HOME` read.
#[cfg(feature = "claude")]
pub(crate) fn cc_config_dir() -> CcConfigDir {
    cc_config_dir_from(std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(), dirs::home_dir())
}

/// Pure core of [`cc_config_dir`], split so the env-based arms are unit-testable
/// (the repo's `xdg_or_home_config` pattern).
#[cfg(feature = "claude")]
fn cc_config_dir_from(override_: Option<&OsStr>, home: Option<PathBuf>) -> CcConfigDir {
    match override_ {
        Some(dir) if !dir.is_empty() => CcConfigDir::Dir(PathBuf::from(dir)),
        Some(_) => CcConfigDir::EmptyOverride,
        None => match home {
            Some(home) => CcConfigDir::Dir(home.join(".claude")),
            None => CcConfigDir::NoHome,
        },
    }
}

/// One install row of CC's `installed_plugins.json` for `<name>@<marketplace>`.
#[cfg(feature = "claude")]
pub(crate) struct InstalledEntry {
    pub version: Option<String>,
    pub install_path: Option<String>,
}

/// The user-scope `installed_plugins.json` install row for `id` (the only scope
/// the classifier reads). `Ok(None)` = no such row (a missing registry file
/// reads as nothing installed); `Err` = the registry is present but unparseable,
/// or present in a schema this reader does not recognize (a `version` other than
/// 2, or a non-array/non-object install entry) — both "cannot tell", so a future
/// schema bump never reads as absent.
#[cfg(feature = "claude")]
pub(crate) fn installed_entry(config_dir: &Path, id: &str) -> Result<Option<InstalledEntry>, String> {
    let path = config_dir.join("plugins").join("installed_plugins.json");
    let root = read_json(&path)?;
    if root.is_null() {
        // A missing registry file reads as nothing installed (`read_json` maps
        // NotFound to null); a present-but-foreign shape below is "cannot tell".
        return Ok(None);
    }
    if root.get("version").and_then(Value::as_u64) != Some(2) {
        return Err("unrecognized installed_plugins.json schema".into());
    }
    let Some(plugins) = root.get("plugins").and_then(Value::as_object) else {
        return Err("unrecognized installed_plugins.json schema".into());
    };
    let Some(rows) = plugins.get(id) else {
        return Ok(None);
    };
    let Some(rows) = rows.as_array() else {
        return Err("unrecognized installed_plugins.json schema".into());
    };
    if rows.iter().any(|row| !row.is_object()) {
        return Err("unrecognized installed_plugins.json schema".into());
    }
    let Some(row) = rows.iter().find(|row| row_at_user_scope(row)) else {
        return Ok(None);
    };
    Ok(Some(InstalledEntry {
        version: row.get("version").and_then(Value::as_str).map(str::to_string),
        install_path: row.get("installPath").and_then(Value::as_str).map(str::to_string),
    }))
}

/// The [`MarketplaceEntry`] `marketplace_health` takes, built from CC's
/// `known_marketplaces.json`. `Ok(None)` = the marketplace is absent; `Err` = the
/// registry is present but unparseable, or a present entry is not an object
/// (cannot tell).
#[cfg(feature = "claude")]
pub(crate) fn marketplace_entry(config_dir: &Path, marketplace: &str) -> Result<Option<MarketplaceEntry>, String> {
    let path = config_dir.join("plugins").join("known_marketplaces.json");
    let root = read_json(&path)?;
    let Some(entry) = root.get(marketplace) else {
        return Ok(None);
    };
    if !entry.is_object() {
        return Err("unrecognized known_marketplaces.json schema".into());
    }
    let source = entry.get("source");
    Ok(Some(MarketplaceEntry {
        name: Some(marketplace.to_string()),
        path: source.and_then(|s| s.get("path")).and_then(Value::as_str).map(str::to_string),
        source: source.and_then(|s| s.get("source")).and_then(Value::as_str).map(str::to_string),
        ref_: None,
        install_location: entry.get("installLocation").and_then(Value::as_str).map(str::to_string),
    }))
}

/// The enable bit for `id`, read as the cwd-merged value CC's `plugin list
/// --json` reports (probed 2.1.310): the first explicit `enabledPlugins[<id>]`
/// wins across `<cwd>/.claude/settings.local.json`, `<cwd>/.claude/settings.json`,
/// then `<config_dir>/settings.json`; none = disabled (a removed key reads
/// `enabled: false`). `Err` = a present layer is unparseable (cannot tell).
#[cfg(feature = "claude")]
pub(crate) fn enabled_plugin(cwd: &Path, config_dir: &Path, id: &str) -> Result<bool, String> {
    for path in
        [cwd.join(".claude").join("settings.local.json"), cwd.join(".claude").join("settings.json"), config_dir.join("settings.json")]
    {
        let root = read_json(&path)?;
        if let Some(enabled) = root.get("enabledPlugins").and_then(Value::as_object).and_then(|m| m.get(id)).and_then(Value::as_bool) {
            return Ok(enabled);
        }
    }
    Ok(false)
}

/// Read + parse a registry file. A missing file reads as empty JSON (absence, not
/// an error); a present-but-unparseable file is the one "cannot tell" shape, since
/// nothing can say what the registry holds.
#[cfg(feature = "claude")]
fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Null),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    serde_json::from_slice(&bytes).map_err(|e| format!("parsing {}: {e}", path.display()))
}

/// Whether one install row is a user-scope row (the only scope the classifier
/// reads): the `scope` field reads `"user"` or is missing (tolerant serde).
#[cfg(feature = "claude")]
fn row_at_user_scope(row: &Value) -> bool {
    row.get("scope").and_then(Value::as_str).is_none_or(|s| s == "user")
}

#[cfg(all(test, any(feature = "omp", feature = "cursor")))]
#[path = "../../tests/unit/ccregistry.rs"]
mod ccregistry_tests;

#[cfg(all(test, feature = "claude"))]
#[path = "../../tests/unit/ccregistry_claude.rs"]
mod ccregistry_claude_tests;
