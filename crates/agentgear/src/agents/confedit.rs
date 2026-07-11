//! Atomic, BOM-free, parent-creating read-modify-write for the harness config
//! files a non-CC backend owns. Two invariants: never clobber a config we could
//! not parse (returns [`Error::Config`] instead of overwriting), and a semantic
//! no-op skips the write so a second reconcile is a true `NoOp`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::{Error, IoContext, Result};

/// Read `path` as JSON (missing -> `{}`), hand the mutable root to `edit`, then
/// write it back pretty + trailing `\n`, no BOM, via temp-then-rename. Creates
/// parents. Returns whether the document changed (drives NoOp vs Installed).
pub(crate) fn json_edit(path: &Path, edit: impl FnOnce(&mut Value) -> Result<()>) -> Result<bool> {
    let mut root = match fs::read(path) {
        // Empty/whitespace-only (a `touch`ed or interrupted-write file) is a common
        // real state and means the same as a missing file: start from `{}`.
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Value::Object(Map::new()),
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| Error::Config { path: path.display().to_string(), detail: e.to_string() })?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(source) => return Err(Error::Io { context: format!("reading {}", path.display()), source }),
    };
    if !root.is_object() {
        // A harness config root is always an object; refuse to overwrite anything else.
        return Err(Error::Config { path: path.display().to_string(), detail: "config root is not a JSON object".into() });
    }

    let before = root.clone();
    edit(&mut root)?;
    // `before` is the parsed existing value or `{}` for a missing file, so an
    // unchanged root also skips creating an empty file.
    if root == before {
        return Ok(false);
    }

    let mut bytes = serde_json::to_vec_pretty(&root).map_err(|source| Error::Json { what: "config".into(), source })?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)?;
    Ok(true)
}

/// Same as [`json_edit`] for TOML via `toml_edit::DocumentMut` (comment/format
/// preserving). Codex-only; the toml editor is gated behind the `codex` feature.
#[cfg(feature = "codex")]
pub(crate) fn toml_edit(path: &Path, edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<()>) -> Result<bool> {
    let existing = match fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => return Err(Error::Io { context: format!("reading {}", path.display()), source }),
    };
    let mut doc: toml_edit::DocumentMut = existing
        .as_deref()
        .unwrap_or("")
        .parse()
        .map_err(|e: toml_edit::TomlError| Error::Config { path: path.display().to_string(), detail: e.to_string() })?;

    edit(&mut doc)?;
    let rendered = doc.to_string();
    let unchanged = match &existing {
        Some(s) => *s == rendered,
        None => rendered.is_empty(),
    };
    if unchanged {
        return Ok(false);
    }
    atomic_write(path, rendered.as_bytes())?;
    Ok(true)
}

/// Ensure `root[path[0]][path[1]]...` is an object and return it, creating (or
/// replacing a non-object at) each intermediate level. We own the key namespace
/// we write into, so replacing a non-object there is intended, not clobbering.
pub(crate) fn json_obj_at<'a>(root: &'a mut Value, path: &[&str]) -> &'a mut Map<String, Value> {
    let mut cur = root;
    for key in path {
        cur = ensure_object(cur).entry((*key).to_string()).or_insert_with(|| Value::Object(Map::new()));
    }
    ensure_object(cur)
}

fn ensure_object(v: &mut Value) -> &mut Map<String, Value> {
    if !matches!(v, Value::Object(_)) {
        *v = Value::Object(Map::new());
    }
    let Value::Object(map) = v else { unreachable!("just set to an object") };
    map
}

/// Write/replace a whole text file idempotently (translated commands/rules/agents).
/// Returns whether it changed. BOM-free, atomic, parents created.
pub(crate) fn write_file_idem(path: &Path, bytes: &[u8]) -> Result<bool> {
    if let Ok(existing) = fs::read(path)
        && existing == bytes
    {
        return Ok(false);
    }
    atomic_write(path, bytes)?;
    Ok(true)
}

/// Remove a file if present (returns whether it existed). For uninstall.
pub(crate) fn remove_file_idem(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(Error::Io { context: format!("removing {}", path.display()), source }),
    }
}

/// Write to a temp sibling then rename onto `path`, so a reader never sees a
/// half-written config and a crash leaves the prior file intact.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).io_ctx(|| format!("creating {}", parent.display()))?;
    }
    let tmp = tmp_sibling(path);
    fs::write(&tmp, bytes).io_ctx(|| format!("writing {}", tmp.display()))?;
    if let Err(source) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(Error::Io { context: format!("renaming {} -> {}", tmp.display(), path.display()), source });
    }
    Ok(())
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".tmp.{:016x}", fastrand::u64(..)));
    path.with_file_name(name)
}

#[cfg(test)]
#[path = "../../tests/unit/confedit.rs"]
mod confedit_tests;
