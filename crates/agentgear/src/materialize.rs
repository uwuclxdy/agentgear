//! Materialize: lay a plugin tree down as a content-keyed versioned dir and flip
//! an atomic `current` pointer at it, so `claude plugin marketplace add
//! <root>/current` always sees a complete tree and a crash mid-way leaves the
//! prior `current` intact.
//!
//! The tree comes from one of two [`TreeSource`]s: the compile-time compressed
//! blob baked into the binary (`Source::Embedded`, a `.tar.br` the build.rs
//! produced) or an on-disk directory (`Source::Path`). Both flatten to the same
//! `(rel-path, bytes)` entries before the write, so the rest of the pipeline is
//! source-agnostic.
//!
//! ```text
//! <data_root>/
//!   versions/<version>/         full tree + generated .claude-plugin/marketplace.json
//!   current -> versions/<version>   symlink (unix) / junction (windows)
//! ```

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, IoContext, Result};
use crate::host::{Plugin, data_root};
use crate::manifest::{MarketplaceManifest, MarketplacePlugin, PluginManifest};
use crate::util::hex;

/// The generated file is excluded from tree hashing: it is a derived artifact, so
/// a source tree (which ships only `plugin.json`) and a materialized tree (which
/// also holds the generated one) must hash equal.
const GENERATED_MARKETPLACE: &str = ".claude-plugin/marketplace.json";

/// Where a materialize reads its plugin tree from. `Blob` is the compile-time
/// `.tar.br` baked into the binary (`Source::Embedded`); `Dir` is an on-disk
/// plugin tree (`Source::Path`).
pub(crate) enum TreeSource<'a> {
    Blob(&'a [u8]),
    Dir(&'a Path),
}

impl TreeSource<'_> {
    /// The tree flattened to `(relative-path, bytes)` file entries, ready to write.
    fn entries(&self) -> Result<Vec<(String, Vec<u8>)>> {
        match self {
            TreeSource::Blob(blob) => blob_entries(blob),
            TreeSource::Dir(dir) => dir_entries(dir),
        }
    }
}

/// Ensure `versions/<version>/` exists with the full tree + generated marketplace,
/// then point `current` at it. Returns the `current` pointer path to hand to
/// `marketplace add`. Idempotent: an existing version dir is reused (dedup across
/// coexisting binaries), and its tree is not re-read (the blob is only
/// decompressed when a write is actually needed).
pub(crate) fn materialize(plugin: &Plugin, tree: TreeSource<'_>) -> Result<PathBuf> {
    let version = plugin.version;
    let unsafe_segment =
        |c: char| c.is_whitespace() || c.is_control() || std::path::is_separator(c) || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*');
    if version.is_empty() || version.contains(unsafe_segment) || version.ends_with('.') {
        return Err(Error::Tree(format!(
            "plugin version {version:?} is not a safe path segment (whitespace, control, separators, `:<>\"|?*`, or a trailing dot are illegal/undeletable on windows)"
        )));
    }

    let root = data_root(plugin)?;
    let versions = root.join("versions");
    fs::create_dir_all(&versions).io_ctx(|| format!("creating {}", versions.display()))?;

    let version_dir = versions.join(version);
    if !version_dir.exists() {
        let entries = tree.entries()?;
        write_version_dir(plugin, &entries, &versions, &version_dir)?;
    }

    flip_pointer(&root, version, &version_dir)?;
    Ok(root.join("current"))
}

/// Write the tree into a temp sibling, then atomically rename onto the versioned
/// target. The target is created exactly once and never renamed onto while
/// non-empty, so `ENOTEMPTY` cannot happen on the happy path.
fn write_version_dir(plugin: &Plugin, entries: &[(String, Vec<u8>)], versions: &Path, version_dir: &Path) -> Result<()> {
    let tmp = versions.join(format!("{}.tmp.{}", plugin.version, rand_suffix()));
    fs::create_dir_all(&tmp).io_ctx(|| format!("creating {}", tmp.display()))?;

    let result = (|| {
        write_entries(entries, &tmp)?;
        let mkt = generate_marketplace(plugin, entries)?;
        let mkt_path = tmp.join(GENERATED_MARKETPLACE);
        if let Some(parent) = mkt_path.parent() {
            fs::create_dir_all(parent).io_ctx(|| format!("creating {}", parent.display()))?;
        }
        write_no_bom(&mkt_path, &mkt)?;
        fsync_dir(&tmp);
        Ok(())
    })();

    if let Err(e) = result {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }

    if let Err(e) = fs::rename(&tmp, version_dir) {
        // Lost a race: another process built the same version dir first. Its tree
        // is byte-identical (same version), so drop ours and reuse theirs.
        let _ = fs::remove_dir_all(&tmp);
        if !version_dir.exists() {
            return Err(Error::Io { context: format!("renaming {} -> {}", tmp.display(), version_dir.display()), source: e });
        }
    }
    // Persist the new dirent before `current` can point at it.
    fsync_dir(versions);
    Ok(())
}

fn write_entries(entries: &[(String, Vec<u8>)], dest: &Path) -> Result<()> {
    for (rel, bytes) in entries {
        let path = dest.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).io_ctx(|| format!("creating {}", parent.display()))?;
        }
        write_no_bom(&path, bytes)?;
    }
    Ok(())
}

/// Write bytes verbatim and fsync the file data. Neither brotli/tar output nor an
/// on-disk tree carries a UTF-8 BOM (which `claude plugin validate` rejects on
/// windows); the sync makes the contents durable before the version-dir rename, so
/// a power-loss cannot leave a `current` pointer at a zero-length tree.
fn write_no_bom(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = File::create(path).io_ctx(|| format!("creating {}", path.display()))?;
    f.write_all(bytes).io_ctx(|| format!("writing {}", path.display()))?;
    f.sync_all().io_ctx(|| format!("syncing {}", path.display()))?;
    Ok(())
}

fn generate_marketplace(plugin: &Plugin, entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let manifest = read_plugin_manifest(entries)?;
    let owner = manifest
        .author
        .ok_or_else(|| {
            Error::Tree("plugin.json needs an `author` (used as the marketplace `owner`; `validate --strict` requires it)".into())
        })?
        .into_person();
    let description = manifest.description.clone().unwrap_or_else(|| format!("{} plugin", plugin.name));
    let mkt = MarketplaceManifest {
        name: plugin.marketplace.to_string(),
        description: description.clone(),
        owner,
        plugins: vec![MarketplacePlugin { name: plugin.name.to_string(), source: "./".to_string(), description }],
    };
    let mut bytes = serde_json::to_vec_pretty(&mkt).map_err(|source| Error::Json { what: "generated marketplace.json".into(), source })?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Read the shipped `plugin.json` out of the flattened tree entries.
pub(crate) fn read_plugin_manifest(entries: &[(String, Vec<u8>)]) -> Result<PluginManifest> {
    let bytes = entries
        .iter()
        .find(|(rel, _)| is_plugin_json(rel))
        .map(|(_, b)| b)
        .ok_or_else(|| Error::Tree("plugin tree has no .claude-plugin/plugin.json".into()))?;
    serde_json::from_slice(bytes).map_err(|source| Error::Json { what: "plugin.json".into(), source })
}

fn is_plugin_json(rel: &str) -> bool {
    let rel = rel.replace('\\', "/");
    rel == ".claude-plugin/plugin.json" || rel.ends_with("/.claude-plugin/plugin.json")
}

// --- tree sources ------------------------------------------------------------

/// Flatten a non-github source to `(rel-path, bytes)` entries for the components
/// IR (`Embedded` -> the baked blob, `Path` -> the on-disk tree). GitHub is not a
/// materializable local tree for non-CC backends in v1.
///
/// `allow(dead_code)`: wired by the per-harness backends + doctor in pass B.
#[allow(dead_code)]
pub(crate) fn entries_for(plugin: &Plugin, source: &crate::host::Source) -> Result<Vec<(String, Vec<u8>)>> {
    use crate::host::Source;
    match source {
        Source::Embedded => blob_entries(plugin.blob()),
        Source::Path(dir) => dir_entries(dir),
        Source::GitHub { .. } => {
            Err(Error::Tree("github source is unsupported for non-Claude backends in v1; use Source::Embedded or Source::Path".into()))
        }
    }
}

/// Decompress the embedded `.tar.br` blob and flatten it to file entries. Feature
/// `embed` gates the brotli/tar deps; without it this errors (a `default-features
/// = false` host cannot use `Source::Embedded`).
#[cfg(feature = "embed")]
pub(crate) fn blob_entries(blob: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    use std::io::Read;

    if blob.is_empty() {
        return Err(Error::Tree(
            "embedded blob is empty: the derive's `embed` attr is off but `Source::Embedded` was requested (use `Source::Path`/`Source::GitHub` or turn `embed` on)".into(),
        ));
    }
    let mut tar_bytes = Vec::new();
    brotli::Decompressor::new(std::io::Cursor::new(blob), 4096)
        .read_to_end(&mut tar_bytes)
        .io_ctx(|| "brotli-decompressing the embedded plugin blob".to_string())?;

    let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
    let mut out = Vec::new();
    for entry in archive.entries().io_ctx(|| "reading the embedded plugin tar".to_string())? {
        let mut entry = entry.io_ctx(|| "reading a plugin tar entry".to_string())?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let raw = entry.path().io_ctx(|| "reading a plugin tar entry path".to_string())?;
        let rel = normalize_rel(&raw.to_string_lossy())?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).io_ctx(|| format!("reading plugin tar entry {rel}"))?;
        out.push((rel, bytes));
    }
    Ok(out)
}

#[cfg(not(feature = "embed"))]
pub(crate) fn blob_entries(_blob: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    Err(Error::Tree("this binary was built without the `embed` feature; use `Source::Path` or `Source::GitHub`, or enable `embed`".into()))
}

/// Read an on-disk plugin tree (`Source::Path`) into file entries.
fn dir_entries(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    if !dir.join(".claude-plugin").join("plugin.json").exists() {
        return Err(Error::Tree(format!("{} is not a plugin tree (no .claude-plugin/plugin.json)", dir.display())));
    }
    let mut out = Vec::new();
    collect_dir(dir, dir, &mut out)?;
    Ok(out)
}

fn collect_dir(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Result<()> {
    for entry in fs::read_dir(dir).io_ctx(|| format!("reading {}", dir.display()))? {
        let entry = entry.io_ctx(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();
        let ft = entry.file_type().io_ctx(|| format!("stat {}", path.display()))?;
        if ft.is_dir() {
            collect_dir(root, &path, out)?;
        } else if ft.is_file() {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            let bytes = fs::read(&path).io_ctx(|| format!("reading {}", path.display()))?;
            out.push((rel, bytes));
        }
    }
    Ok(())
}

/// Normalize a tar entry path to a forward-slash relative path, rejecting any
/// `..`/absolute segment so a corrupt blob cannot write outside the temp dir.
#[cfg(feature = "embed")]
fn normalize_rel(rel: &str) -> Result<String> {
    let rel = rel.replace('\\', "/");
    let rel = rel.trim_start_matches("./").trim_start_matches('/');
    if rel.is_empty() || rel.split('/').any(|c| c == "..") {
        return Err(Error::Tree(format!("plugin tree entry {rel:?} escapes the tree (`..` or absolute path)")));
    }
    Ok(rel.to_string())
}

// --- compression (build.rs helper + tests) -----------------------------------

/// Tar the plugin tree at `dir` and brotli-compress it into a `.tar.br` blob. Used
/// by `build::assert_plugin_version` to bake the embedded blob; deterministic
/// (sorted walk, contents only, no mtime).
#[cfg(feature = "embed")]
pub(crate) fn compress_dir(dir: &Path) -> Result<Vec<u8>> {
    if !dir.join(".claude-plugin").join("plugin.json").exists() {
        return Err(Error::Tree(format!("{} is not a plugin tree (no .claude-plugin/plugin.json)", dir.display())));
    }
    let mut builder = tar::Builder::new(Vec::new());
    append_tree(&mut builder, dir, dir)?;
    let tar_bytes = builder.into_inner().io_ctx(|| "finishing the plugin tar".to_string())?;

    let mut out = Vec::new();
    {
        // quality 11 (max ratio), lgwin 22 (max window); one-time per tree change.
        let mut w = brotli::CompressorWriter::new(&mut out, 4096, 11, 22);
        w.write_all(&tar_bytes).io_ctx(|| "brotli-compressing the plugin tar".to_string())?;
    }
    Ok(out)
}

#[cfg(feature = "embed")]
fn append_tree(builder: &mut tar::Builder<Vec<u8>>, base: &Path, dir: &Path) -> Result<()> {
    let mut paths: Vec<PathBuf> =
        fs::read_dir(dir).io_ctx(|| format!("reading {}", dir.display()))?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        let meta = fs::symlink_metadata(&path).io_ctx(|| format!("stat {}", path.display()))?;
        if meta.is_dir() {
            append_tree(builder, base, &path)?;
        } else if meta.is_file() {
            let rel = path.strip_prefix(base).unwrap_or(&path);
            let data = fs::read(&path).io_ctx(|| format!("reading {}", path.display()))?;
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            builder.append_data(&mut header, rel, data.as_slice()).io_ctx(|| format!("adding {} to the plugin tar", path.display()))?;
        }
    }
    Ok(())
}

// --- pointer flip ------------------------------------------------------------

fn flip_pointer(root: &Path, version: &str, version_dir: &Path) -> Result<()> {
    let current = root.join("current");
    let rel_target = Path::new("versions").join(version);
    make_pointer(root, &current, &rel_target, version_dir)
}

#[cfg(unix)]
fn make_pointer(root: &Path, current: &Path, rel_target: &Path, _abs_target: &Path) -> Result<()> {
    let tmp = root.join(format!("current.tmp.{}", rand_suffix()));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(rel_target, &tmp).io_ctx(|| format!("linking {}", tmp.display()))?;
    fs::rename(&tmp, current).io_ctx(|| format!("flipping {}", current.display()))?;
    fsync_dir(root);
    Ok(())
}

#[cfg(windows)]
fn make_pointer(_root: &Path, current: &Path, _rel_target: &Path, abs_target: &Path) -> Result<()> {
    // Junctions require an absolute target and cannot be atomically renamed over a
    // dir reparse point, so replace in place. Windows is designed-in, not CI-gated.
    // Use `symlink_metadata` (not `exists`, which follows the reparse point) so a
    // *dangling* junction — target gone — is still detected and cleared.
    if fs::symlink_metadata(current).is_ok() {
        let _ = junction::delete(current);
        let _ = fs::remove_dir(current);
    }
    junction::create(abs_target, current).io_ctx(|| format!("creating junction {}", current.display()))?;
    Ok(())
}

#[cfg(unix)]
fn fsync_dir(path: &Path) {
    // Best-effort durability of the rename; not required for the atomicity itself.
    if let Ok(f) = File::open(path) {
        let _ = f.sync_all();
    }
}

#[cfg(not(unix))]
fn fsync_dir(_path: &Path) {}

fn rand_suffix() -> String {
    format!("{:016x}.{}", fastrand::u64(..), std::process::id())
}

// --- content hashing ---------------------------------------------------------

/// Stable hash of the embedded source tree (excludes the generated marketplace),
/// for the doctor check that `current` has not gone stale or corrupt. Decompresses
/// the blob first, so it errors without the `embed` feature.
pub(crate) fn tree_hash(blob: &[u8]) -> Result<String> {
    let entries = blob_entries(blob)?;
    let mut files: Vec<(String, &[u8])> =
        entries.iter().filter(|(rel, _)| rel != GENERATED_MARKETPLACE).map(|(rel, bytes)| (rel.clone(), bytes.as_slice())).collect();
    Ok(hash_pairs(&mut files))
}

/// Stable hash of a materialized tree on disk (same exclusion), for the doctor
/// check that `current` matches its source tree.
pub(crate) fn dir_hash(root: &Path) -> Result<String> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    collect_disk(root, root, &mut files)?;
    let mut refs: Vec<(String, &[u8])> = files.iter().map(|(p, b)| (p.clone(), b.as_slice())).collect();
    Ok(hash_pairs(&mut refs))
}

fn collect_disk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Result<()> {
    let entries = fs::read_dir(dir).io_ctx(|| format!("reading {}", dir.display()))?;
    for entry in entries {
        let entry = entry.io_ctx(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();
        let ft = entry.file_type().io_ctx(|| format!("stat {}", path.display()))?;
        if ft.is_dir() {
            collect_disk(root, &path, out)?;
        } else if ft.is_file() {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if rel != GENERATED_MARKETPLACE {
                let bytes = fs::read(&path).io_ctx(|| format!("reading {}", path.display()))?;
                out.push((rel, bytes));
            }
        }
    }
    Ok(())
}

fn hash_pairs(files: &mut [(String, &[u8])]) -> String {
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (path, bytes) in files.iter() {
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    hex(&hasher.finalize())
}

#[cfg(all(test, feature = "embed"))]
#[path = "../tests/unit/materialize.rs"]
mod materialize_tests;
