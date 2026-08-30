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
//! The staging is client-scoped: CC and copilot copy the resulting tree verbatim
//! into their own plugin caches and run its hooks, so the `${AGENTGEAR_CLIENT}`
//! token is substituted per client into the tree before it is written, and the
//! version dir + pointer carry a `@<client>` suffix so two plugin-native backends
//! never collide on one shared dir.
//!
//! ```text
//! <data_root>/
//!   versions/<version>-<hash>@<client>/   full tree + generated .claude-plugin/marketplace.json
//!   current@<client> -> versions/<version>-<hash>@<client>   symlink (unix) / junction (windows)
//! ```
//!
//! `<hash>` is the first 16 hex chars of the tree hash the client would materialize,
//! so a tree edited at an UNCHANGED version lands in a new dir and the pointer flips
//! onto it. Keying the dir on the version alone made the write skip on any tree the
//! version had already staged, which left a box serving whatever bytes that version
//! first shipped.

use std::fs;
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
use std::fs::File;
// Every build that WRITES a tree: `embed` bakes one at build time (`compress_dir`),
// a plugin-native backend stages one at install time (`materialize`). Reading a tree
// (`dir_entries`, the components IR) needs neither.
#[cfg(any(feature = "embed", feature = "claude", feature = "copilot-cli"))]
use std::io::Write;
use std::path::Path;
#[cfg(any(feature = "embed", feature = "claude", feature = "copilot-cli"))]
use std::path::PathBuf;

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
use sha2::{Digest, Sha256};

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
use crate::components::AGENTGEAR_CLIENT_TOKEN;
use crate::error::{Error, IoContext, Result};
use crate::host::Plugin;
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
use crate::host::data_root;
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
use crate::manifest::{MarketplaceManifest, MarketplacePlugin, PluginManifest};
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
use crate::util::hex;

/// The generated file is excluded from tree hashing: it is a derived artifact, so
/// a source tree (which ships only `plugin.json`) and a materialized tree (which
/// also holds the generated one) must hash equal.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
const GENERATED_MARKETPLACE: &str = ".claude-plugin/marketplace.json";

/// Where a materialize reads its plugin tree from. `Blob` is the compile-time
/// `.tar.br` baked into the binary (`Source::Embedded`); `Dir` is an on-disk
/// plugin tree (`Source::Path`).
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
pub(crate) enum TreeSource<'a> {
    Blob(&'a [u8]),
    Dir(&'a Path),
}

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
impl TreeSource<'_> {
    /// The tree flattened to `(relative-path, bytes)` file entries, ready to write.
    fn entries(&self) -> Result<Vec<(String, Vec<u8>)>> {
        match self {
            TreeSource::Blob(blob) => blob_entries(blob),
            TreeSource::Dir(dir) => dir_entries(dir),
        }
    }
}

/// Ensure `versions/<version>-<hash>@<client>/` exists with the full tree + generated
/// marketplace, then point `current@<client>` at it. Returns the `current` pointer path
/// to hand to `marketplace add`. Idempotent on unchanged bytes: the same tree hashes to
/// the same dir name, which is reused (dedup across coexisting binaries) and never
/// rewritten. Changed bytes at the same version hash differently, so they get their own
/// dir and the pointer flips onto it.
///
/// The tree is therefore always read (the blob decompressed) to compute that hash,
/// where version-keying could skip it. That is the price of the content key: a hash
/// derived from anything cheaper than the bytes cannot see a same-version edit.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
pub(crate) fn materialize(plugin: &Plugin, tree: TreeSource<'_>, client_id: &str) -> Result<PathBuf> {
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

    let mut entries = tree.entries()?;
    // Client-scope the staging: each plugin-native backend bakes its own id into
    // the tree it copies and runs, so the shared dir can't collide between them.
    expand_client_entries(&mut entries, client_id);
    let dir_name = version_dir_name(version, &hash_entries(&entries), client_id);
    let version_dir = versions.join(&dir_name);
    if !version_dir.exists() {
        write_version_dir(plugin, &entries, &versions, &version_dir)?;
    }

    flip_pointer(&root, &dir_name, client_id, &version_dir)?;
    prune_superseded(&versions, version, client_id, &dir_name);
    Ok(root.join(format!("current@{client_id}")))
}

/// `<version>-<hash prefix>@<client>`. 64 bits of the tree hash: enough that two
/// distinct trees never share a dir, short enough to keep the path readable.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn version_dir_name(version: &str, content_hash: &str, client_id: &str) -> String {
    format!("{version}-{}@{client_id}", &content_hash[..VERSION_DIR_HASH_LEN])
}

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
const VERSION_DIR_HASH_LEN: usize = 16;

/// Delete this version's other content variants for this client, best-effort, once the
/// pointer no longer names them. Without it every same-version tree edit leaks a full
/// copy of the tree, which is routine on a host still in development rather than the
/// crash-only leak the `.tmp.<rand>` dirs are.
///
/// Scoped hard: only `<version>@<client>` (what pre-content-keying binaries wrote) and
/// `<version>-<16 hex>@<client>`. A pre-release version is a prefix of nothing here —
/// `0.1.0` never matches `0.1.0-rc.1-<hash>@<client>`, whose remainder is not 16 hex
/// chars — and another version's dirs are left alone, so a rollback still finds its tree.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn prune_superseded(versions: &Path, version: &str, client_id: &str, keep: &str) {
    let suffix = format!("@{client_id}");
    let Ok(read_dir) = fs::read_dir(versions) else {
        return;
    };
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == keep {
            continue;
        }
        let Some(stem) = name.strip_suffix(&suffix) else {
            continue;
        };
        let superseded = match stem.strip_prefix(version) {
            Some("") => true,
            Some(rest) => {
                rest.len() == VERSION_DIR_HASH_LEN + 1 && rest.starts_with('-') && rest[1..].bytes().all(|b| b.is_ascii_hexdigit())
            }
            None => false,
        };
        if superseded && entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Substitute the ASCII [`AGENTGEAR_CLIENT_TOKEN`] with `client` across every file's
/// bytes. The token is pure ASCII, so a byte-level replace never corrupts UTF-8 or
/// binary content. The generated marketplace is produced fresh (not part of these
/// entries) and never carries the token, so it is excluded the same way it is from
/// hashing; when the token is absent every file is left untouched.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn expand_client_entries(entries: &mut [(String, Vec<u8>)], client: &str) {
    let needle = AGENTGEAR_CLIENT_TOKEN.as_bytes();
    let repl = client.as_bytes();
    for (rel, bytes) in entries.iter_mut() {
        if rel != GENERATED_MARKETPLACE && contains_subslice(bytes, needle) {
            *bytes = replace_subslice(bytes, needle, repl);
        }
    }
}

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn replace_subslice(haystack: &[u8], needle: &[u8], repl: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i..].starts_with(needle) {
            out.extend_from_slice(repl);
            i += needle.len();
        } else {
            out.push(haystack[i]);
            i += 1;
        }
    }
    out
}

/// Write the tree into a temp sibling, then atomically rename onto the versioned
/// target. The target is created exactly once and never renamed onto while
/// non-empty, so `ENOTEMPTY` cannot happen on the happy path.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
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
        // is byte-identical (the dir name carries the content hash), so drop ours
        // and reuse theirs.
        let _ = fs::remove_dir_all(&tmp);
        if !version_dir.exists() {
            return Err(Error::Io { context: format!("renaming {} -> {}", tmp.display(), version_dir.display()), source: e });
        }
    }
    // Persist the new dirent before `current` can point at it.
    fsync_dir(versions);
    Ok(())
}

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
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
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn write_no_bom(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = File::create(path).io_ctx(|| format!("creating {}", path.display()))?;
    f.write_all(bytes).io_ctx(|| format!("writing {}", path.display()))?;
    f.sync_all().io_ctx(|| format!("syncing {}", path.display()))?;
    Ok(())
}

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
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
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
pub(crate) fn read_plugin_manifest(entries: &[(String, Vec<u8>)]) -> Result<PluginManifest> {
    let bytes = entries
        .iter()
        .find(|(rel, _)| is_plugin_json(rel))
        .map(|(_, b)| b)
        .ok_or_else(|| Error::Tree("plugin tree has no .claude-plugin/plugin.json".into()))?;
    serde_json::from_slice(bytes).map_err(|source| Error::Json { what: "plugin.json".into(), source })
}

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn is_plugin_json(rel: &str) -> bool {
    let rel = rel.replace('\\', "/");
    rel == ".claude-plugin/plugin.json" || rel.ends_with("/.claude-plugin/plugin.json")
}

// --- tree sources ------------------------------------------------------------

/// Flatten a non-github source to `(rel-path, bytes)` entries for the components
/// IR (`Embedded` -> the baked blob, `Path` -> the on-disk tree). GitHub is not a
/// materializable local tree for non-CC backends in v1.
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

/// Read an on-disk plugin tree (`Source::Path`) into file entries. Also the
/// build-time portability lint's tree reader (`build::warn_non_portable`), so it
/// stays available without the `embed` feature.
pub(crate) fn dir_entries(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
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

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn flip_pointer(root: &Path, dir_name: &str, client_id: &str, version_dir: &Path) -> Result<()> {
    let current = root.join(format!("current@{client_id}"));
    let rel_target = Path::new("versions").join(dir_name);
    make_pointer(root, &current, &rel_target, version_dir)
}

#[cfg(unix)]
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn make_pointer(root: &Path, current: &Path, rel_target: &Path, _abs_target: &Path) -> Result<()> {
    let tmp = root.join(format!("current.tmp.{}", rand_suffix()));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(rel_target, &tmp).io_ctx(|| format!("linking {}", tmp.display()))?;
    fs::rename(&tmp, current).io_ctx(|| format!("flipping {}", current.display()))?;
    fsync_dir(root);
    Ok(())
}

#[cfg(windows)]
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
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
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn fsync_dir(path: &Path) {
    // Best-effort durability of the rename; not required for the atomicity itself.
    if let Ok(f) = File::open(path) {
        let _ = f.sync_all();
    }
}

#[cfg(not(unix))]
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn fsync_dir(_path: &Path) {}

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn rand_suffix() -> String {
    format!("{:016x}.{}", fastrand::u64(..), std::process::id())
}

// --- content hashing ---------------------------------------------------------

/// Stable hash of a source tree AS MATERIALIZED for `client` (token-substituted,
/// generated marketplace excluded). Three callers key on the same bytes: the version
/// dir's own name, the doctor check that `current@<client>` has not gone stale or
/// corrupt, and the plugin-native backends' staleness gate (the hash their stamp
/// marker records against what the harness was last handed). When the token is absent
/// the substitution is a no-op, so the hash equals the raw tree's. A `Blob` source is
/// decompressed first, so it errors without the `embed` feature.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
pub(crate) fn content_hash(tree: TreeSource<'_>, client: &str) -> Result<String> {
    let mut entries = tree.entries()?;
    expand_client_entries(&mut entries, client);
    Ok(hash_entries(&entries))
}

/// Hash flattened entries (generated marketplace excluded), for the client-scoped
/// baselines that compare against a materialized `current@<client>` tree.
#[cfg(any(feature = "claude", feature = "copilot-cli"))]
fn hash_entries(entries: &[(String, Vec<u8>)]) -> String {
    let mut files: Vec<(String, &[u8])> =
        entries.iter().filter(|(rel, _)| rel != GENERATED_MARKETPLACE).map(|(rel, bytes)| (rel.clone(), bytes.as_slice())).collect();
    hash_pairs(&mut files)
}

/// Stable hash of a materialized tree on disk (same exclusion), for the doctor
/// check that `current` matches its source tree.
#[cfg(feature = "claude")]
pub(crate) fn dir_hash(root: &Path) -> Result<String> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    collect_disk(root, root, &mut files)?;
    let mut refs: Vec<(String, &[u8])> = files.iter().map(|(p, b)| (p.clone(), b.as_slice())).collect();
    Ok(hash_pairs(&mut refs))
}

#[cfg(feature = "claude")]
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

#[cfg(any(feature = "claude", feature = "copilot-cli"))]
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

// `embed` for the blob helpers, `claude` for the hash surface the tree-hash
// equivalence tests assert on; both are the gates the items under test carry.
#[cfg(all(test, feature = "embed", feature = "claude"))]
#[path = "../tests/unit/materialize.rs"]
mod materialize_tests;
