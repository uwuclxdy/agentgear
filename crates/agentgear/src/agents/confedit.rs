//! Atomic, BOM-free, parent-creating read-modify-write for the harness config
//! files a non-CC backend owns. Two invariants: never clobber a config we could
//! not parse (returns [`Error::Config`] instead of overwriting), and a semantic
//! no-op skips the write so a second reconcile is a true `NoOp`.
//!
//! Removal paths take a second pair ([`json_remove`] + [`json_prune_obj`]) that undoes
//! what the creating write laid down, so an uninstall leaves the file as it found it
//! rather than a shell of the containers we made. `yaml_prune_map` is the YAML half of
//! the container rule; the file-delete half stays JSON-only, since a YAML config can
//! carry comments the user would lose with it.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::{Error, IoContext, Result};

/// Read `path` as JSON (missing -> `{}`), hand the mutable root to `edit`, then
/// write it back pretty + trailing `\n`, no BOM, via temp-then-rename. Creates
/// parents. Returns whether the document changed (drives NoOp vs Installed).
pub(crate) fn json_edit(path: &Path, edit: impl FnOnce(&mut Value) -> Result<()>) -> Result<bool> {
    json_write(path, false, edit)
}

/// [`json_edit`] for a removal path: identical, except that a root our edit left
/// empty takes the file with it. A root holding nothing once we have taken our own
/// keys back held nothing but them, so dropping it is the exact inverse of the
/// install that created it. Install never routes here, so nothing on that side can
/// delete a file.
pub(crate) fn json_remove(path: &Path, edit: impl FnOnce(&mut Value) -> Result<()>) -> Result<bool> {
    json_write(path, true, edit)
}

fn json_write(path: &Path, drop_empty_root: bool, edit: impl FnOnce(&mut Value) -> Result<()>) -> Result<bool> {
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
    // Unreachable from a missing file: `before` would be `{}` too, so an emptied root
    // equals it and returns above without touching the path.
    if drop_empty_root && root.as_object().is_some_and(Map::is_empty) {
        remove_file_idem(path)?;
        return Ok(true);
    }

    let mut bytes = serde_json::to_vec_pretty(&root).map_err(|source| Error::Json { what: "config".into(), source })?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)?;
    Ok(true)
}

/// Same as [`json_edit`] for TOML via `toml_edit::DocumentMut` (comment/format
/// preserving). Gated on the backends whose configs are TOML (codex's config.toml,
/// kimi's hook-bearing config.toml).
#[cfg(any(feature = "codex", feature = "kimi"))]
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

/// Same as [`json_edit`] for YAML via `serde_norway`. Unlike the toml editor this
/// is NOT comment/format preserving (no comment-preserving YAML editor exists in
/// pure Rust): a write re-renders the document, keeping keys and values but
/// dropping comments, anchors/aliases, and tags. A multi-document (`---`) file
/// fails the single-`Value` parse and surfaces as `Error::Config` (refused, never
/// clobbered). Backends must keep edits semantically no-op-aware so an
/// already-converged config is never rewritten. Goose-only for now.
#[cfg(feature = "goose")]
pub(crate) fn yaml_edit(path: &Path, edit: impl FnOnce(&mut serde_norway::Value) -> Result<()>) -> Result<bool> {
    use serde_norway::{Mapping, Value as Yaml};
    let mut root = match fs::read(path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Yaml::Mapping(Mapping::new()),
        Ok(bytes) => {
            serde_norway::from_slice(&bytes).map_err(|e| Error::Config { path: path.display().to_string(), detail: e.to_string() })?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Yaml::Mapping(Mapping::new()),
        Err(source) => return Err(Error::Io { context: format!("reading {}", path.display()), source }),
    };
    // A YAML `~` (null) document is what an empty-but-commented file parses to;
    // treat it like missing. Any other non-mapping root is refused, same as JSON.
    if root.is_null() {
        root = Yaml::Mapping(Mapping::new());
    }
    if !root.is_mapping() {
        return Err(Error::Config { path: path.display().to_string(), detail: "config root is not a YAML mapping".into() });
    }

    let before = root.clone();
    edit(&mut root)?;
    if root == before {
        return Ok(false);
    }

    let text = serde_norway::to_string(&root)
        .map_err(|e| Error::Config { path: path.display().to_string(), detail: format!("rendering YAML: {e}") })?;
    atomic_write(path, text.as_bytes())?;
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

/// The removal-side counterpart of [`json_obj_at`]: run `edit` on the value at
/// `path` — navigating WITHOUT creating — then drop every level `edit` left empty.
/// Returns whether anything was dropped.
///
/// Emptiness is measured ACROSS `edit`, not after it. A container already empty when
/// we arrive is the user's own and survives, so a teardown that takes nothing back
/// removes nothing. An ancestor needs no such test: it held the level we just
/// dropped, so it was non-empty by construction.
///
/// `[]` addresses the root, which has no parent to be dropped from — [`json_remove`]
/// is what takes the file itself.
pub(crate) fn json_prune_at(root: &mut Value, path: &[&str], edit: impl FnOnce(&mut Value) -> Result<()>) -> Result<bool> {
    let Some((key, rest)) = path.split_first() else {
        edit(root)?;
        return Ok(false);
    };
    let Some(map) = root.as_object_mut() else { return Ok(false) };
    let Some(child) = map.get_mut(*key) else { return Ok(false) };
    let was_empty = is_empty_container(child);
    let pruned_below = json_prune_at(child, rest, edit)?;
    let now_empty = is_empty_container(child);
    if !was_empty && now_empty {
        map.remove(*key);
        return Ok(true);
    }
    Ok(pruned_below)
}

/// [`json_prune_at`] for the usual case, an object container. A key holding anything
/// else is left untouched, matching what a non-creating `as_object_mut` walk did.
pub(crate) fn json_prune_obj(root: &mut Value, path: &[&str], edit: impl FnOnce(&mut Map<String, Value>) -> Result<()>) -> Result<bool> {
    json_prune_at(root, path, |value| match value.as_object_mut() {
        Some(map) => edit(map),
        None => Ok(()),
    })
}

/// An object or array with no members — the only shapes a removal of ours empties.
fn is_empty_container(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// The YAML twin of [`json_prune_obj`], and the removal-side counterpart of a
/// create-on-the-way-in accessor (goose's `ext_map`): run `edit` on the mapping at
/// `key` — navigating WITHOUT creating — then drop `key` if `edit` left it empty.
/// Returns whether it was dropped.
///
/// Emptiness is measured ACROSS `edit`, exactly as the JSON twin measures it: a
/// mapping already empty when we arrive is the user's own and survives, so a teardown
/// that takes nothing back removes nothing. That only holds while `edit` confines
/// itself to keys we wrote — a sweep inside it empties the mapping on the user's
/// behalf and this guard then reads that as our own doing.
///
/// One level, no path walk: [`json_prune_at`] recurses because its callers address
/// nested containers, and the single YAML caller has one top-level mapping. There is
/// no YAML twin of [`json_remove`] either — a YAML config can carry comments, so
/// taking the file costs more than the empty-`{}` file JSON accepts.
#[cfg(feature = "goose")]
pub(crate) fn yaml_prune_map(
    root: &mut serde_norway::Value, key: &str, edit: impl FnOnce(&mut serde_norway::Mapping) -> Result<()>,
) -> Result<bool> {
    let Some(map) = root.as_mapping_mut() else { return Ok(false) };
    // A key holding anything but a mapping is left untouched, matching the
    // non-creating walk the JSON twin does.
    let Some(child) = map.get_mut(key).and_then(serde_norway::Value::as_mapping_mut) else { return Ok(false) };
    let was_empty = child.is_empty();
    edit(child)?;
    if !was_empty && child.is_empty() {
        map.remove(key);
        return Ok(true);
    }
    Ok(false)
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

/// A YAML scalar for a frontmatter value: bare when it cannot be misparsed as a
/// flow/indicator token, else a double-quoted string with the minimal escapes.
pub(crate) fn yaml_scalar(s: &str) -> String {
    let needs_quote = s.is_empty()
        || s.starts_with(|c: char| c.is_ascii_whitespace())
        || s.ends_with(|c: char| c.is_ascii_whitespace())
        || s.contains(['"', '\\', '\n', '\r', '\t', ':', '#', '[', ']', '{', '}', ',', '&', '*', '!', '|', '>', '\'', '%', '@', '`'])
        || matches!(s.to_ascii_lowercase().as_str(), "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~");
    if !needs_quote {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Unconditionally double-quote a single-line scalar for a YAML frontmatter value,
/// escaping `"` and `\` so an arbitrary value stays safe regardless of colons or
/// quotes. Unlike [`yaml_scalar`], never emits a bare form.
pub(crate) fn yaml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
