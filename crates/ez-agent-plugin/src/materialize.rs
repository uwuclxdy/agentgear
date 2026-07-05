//! Embedded-mode materialize: lay the baked plugin tree down as a content-keyed
//! versioned dir and flip an atomic `current` pointer at it, so `claude plugin
//! marketplace add <root>/current` always sees a complete tree and a crash mid-way
//! leaves the prior `current` intact.
//!
//! ```text
//! <data_root>/
//!   versions/<version>/         full tree + generated .claude-plugin/marketplace.json
//!   current -> versions/<version>   symlink (unix) / junction (windows)
//! ```

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use include_dir::{Dir, DirEntry};
use sha2::{Digest, Sha256};

use crate::error::{Error, IoContext, Result};
use crate::host::{Plugin, data_root};
use crate::manifest::{MarketplaceManifest, MarketplacePlugin, PluginManifest};

/// The generated file is excluded from tree hashing: it is a derived artifact, so
/// an embedded tree (which ships only `plugin.json`) and a materialized tree (which
/// also holds the generated one) must hash equal.
const GENERATED_MARKETPLACE: &str = ".claude-plugin/marketplace.json";

/// Ensure `versions/<version>/` exists with the full tree + generated marketplace,
/// then point `current` at it. Returns the `current` pointer path to hand to
/// `marketplace add`. Idempotent: an existing version dir is reused (dedup across
/// coexisting binaries).
pub(crate) fn materialize(plugin: &Plugin) -> Result<PathBuf> {
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
        write_version_dir(plugin, &versions, &version_dir)?;
    }

    flip_pointer(&root, version, &version_dir)?;
    Ok(root.join("current"))
}

/// Write the tree into a temp sibling, then atomically rename onto the versioned
/// target. The target is created exactly once and never renamed onto while
/// non-empty, so `ENOTEMPTY` cannot happen on the happy path.
fn write_version_dir(plugin: &Plugin, versions: &Path, version_dir: &Path) -> Result<()> {
    let tmp = versions.join(format!("{}.tmp.{}", plugin.version, rand_suffix()));
    fs::create_dir_all(&tmp).io_ctx(|| format!("creating {}", tmp.display()))?;

    let result = (|| {
        write_entries(plugin.tree().entries(), &tmp)?;
        let mkt = generate_marketplace(plugin)?;
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

fn write_entries(entries: &[DirEntry<'_>], dest: &Path) -> Result<()> {
    for entry in entries {
        match entry {
            DirEntry::Dir(dir) => {
                let path = dest.join(dir.path());
                fs::create_dir_all(&path).io_ctx(|| format!("creating {}", path.display()))?;
                write_entries(dir.entries(), dest)?;
            }
            DirEntry::File(file) => {
                let path = dest.join(file.path());
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).io_ctx(|| format!("creating {}", parent.display()))?;
                }
                write_no_bom(&path, file.contents())?;
            }
        }
    }
    Ok(())
}

/// Write bytes verbatim and fsync the file data. serde_json and `include_dir`
/// never prepend a UTF-8 BOM (which `claude plugin validate` rejects on windows);
/// the sync makes the contents durable before the version-dir rename, so a
/// power-loss cannot leave a `current` pointer at a zero-length tree.
fn write_no_bom(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = File::create(path).io_ctx(|| format!("creating {}", path.display()))?;
    f.write_all(bytes).io_ctx(|| format!("writing {}", path.display()))?;
    f.sync_all().io_ctx(|| format!("syncing {}", path.display()))?;
    Ok(())
}

fn generate_marketplace(plugin: &Plugin) -> Result<Vec<u8>> {
    let manifest = read_plugin_manifest(plugin.tree())?;
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

/// Read the shipped `plugin.json` out of the embedded tree.
pub(crate) fn read_plugin_manifest(tree: &Dir<'_>) -> Result<PluginManifest> {
    let file = find_plugin_json(tree).ok_or_else(|| Error::Tree("embedded tree has no .claude-plugin/plugin.json".into()))?;
    serde_json::from_slice(file.contents()).map_err(|source| Error::Json { what: "plugin.json".into(), source })
}

fn find_plugin_json<'a>(tree: &'a Dir<'a>) -> Option<&'a include_dir::File<'a>> {
    fn walk<'a>(entries: &'a [DirEntry<'a>]) -> Option<&'a include_dir::File<'a>> {
        for entry in entries {
            match entry {
                DirEntry::File(f) => {
                    let is_plugin_json = f.path().file_name().is_some_and(|n| n == "plugin.json");
                    let in_claude_plugin = f.path().parent().and_then(Path::file_name).is_some_and(|d| d == ".claude-plugin");
                    if is_plugin_json && in_claude_plugin {
                        return Some(f);
                    }
                }
                DirEntry::Dir(d) => {
                    if let Some(found) = walk(d.entries()) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }
    walk(tree.entries())
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

/// Stable hash of the embedded source tree (excludes the generated marketplace).
pub(crate) fn tree_hash(tree: &Dir<'_>) -> String {
    let mut files: Vec<(String, &[u8])> = Vec::new();
    collect_embedded(tree.entries(), &mut files);
    hash_pairs(&mut files)
}

fn collect_embedded<'a>(entries: &'a [DirEntry<'a>], out: &mut Vec<(String, &'a [u8])>) {
    for entry in entries {
        match entry {
            DirEntry::Dir(d) => collect_embedded(d.entries(), out),
            DirEntry::File(f) => {
                let rel = f.path().to_string_lossy().replace('\\', "/");
                if rel != GENERATED_MARKETPLACE {
                    out.push((rel, f.contents()));
                }
            }
        }
    }
}

/// Stable hash of a materialized tree on disk (same exclusion), for the doctor
/// check that `current` has not gone stale or corrupt versus the embedded tree.
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
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

#[cfg(test)]
#[path = "../tests/unit/materialize.rs"]
mod materialize_tests;
