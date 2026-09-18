//! Host-supplied installPath re-root over Claude Code's `installed_plugins.json`
//! (design §installPath convergence).
//!
//! A host that runs Claude Code against per-session config dirs (each reaching
//! `plugins/` through a symlink onto the shared `~/.claude/plugins`) sees CC
//! record every install's `installPath` *through the session dir*. The session
//! dir dies, the recorded path dangles, and every later session that loads the
//! plugin reports `Plugin "<name>" not cached at <path>`. The CLI has no
//! command that re-spells a recorded path, so the host converges the file
//! itself — the narrow exception to the "never write CC's registry state"
//! rule: only `installPath` spellings the host's own remap targets change, and
//! only to the same directory through the documented symlink. Never a parse,
//! never a reformat: the file is edited at the byte level, so a schema bump
//! renames keys without breaking the edit, and formatting another session
//! wrote survives untouched.

use std::path::Path;

use crate::error::{Error, Result};
use crate::util::atomic_write;

/// How many times a pass restarts on a registry that changed under it before
/// it gives up and reports the contention (design §installPath convergence).
const REPOINT_ATTEMPTS: usize = 3;

/// What a remap decided for one candidate path value.
pub enum Remap {
    /// Not a path this remap targets; leave the bytes alone.
    Keep,
    /// Rewrite the value to this spelling. The spelling lands in the file
    /// verbatim — the remap owns JSON safety: a `to` containing `"` or `\`
    /// would corrupt the registry, so hand it a ready-to-write spelling.
    /// Rewriting a value to itself is a no-op.
    Rewrite(String),
    /// A targeted path this pass cannot converge; named in the report, bytes
    /// left alone.
    Skip(String),
}

/// One recorded path this pass re-rooted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repointed {
    /// The spelling the registry held.
    pub from: String,
    /// The spelling the registry now holds.
    pub to: String,
}

/// A targeted path this pass named and left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepointSkip {
    /// The spelling the registry holds.
    pub path: String,
    /// Why the remap could not converge it.
    pub reason: String,
}

/// What one [`repoint_install_paths`] pass did. Both vectors follow the file's
/// value order; a spelling recorded by several entries is one row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepointReport {
    /// Paths re-rooted, first occurrence of each spelling.
    pub rewritten: Vec<Repointed>,
    /// Paths the remap targeted but could not converge, first occurrence of
    /// each spelling.
    pub skipped: Vec<RepointSkip>,
}

impl RepointReport {
    /// Whether the file was written.
    pub fn changed(&self) -> bool {
        !self.rewritten.is_empty()
    }
}

/// Scan the registry for quoted values, ask `remap` about each, rewrite the
/// targeted ones, and write the file only when something changed. The scan
/// sees keys and values alike — a remap answers [`Remap::Keep`] for anything
/// that is not a path it owns. The file is never parsed and never reformatted:
/// output bytes equal the input bytes except that each rewritten value's
/// interior is the new spelling.
///
/// The registry is shared, so a concurrent writer (a live session's plugin
/// op) can land between the read and the rename; the rename would silently
/// drop it. A pass therefore re-reads before writing and restarts on the
/// fresh bytes, up to [`REPOINT_ATTEMPTS`] attempts, then reports the
/// contention. A registry that vanishes mid-pass is refused, never
/// resurrected: writing the pre-delete image back would undo whoever removed
/// it. An unreadable or non-UTF-8 file is an error; a missing one is not one
/// (a box that never installed a plugin has no registry, and there is nothing
/// to converge).
pub fn repoint_install_paths(registry: &Path, mut remap: impl FnMut(&str) -> Remap) -> Result<RepointReport> {
    for _ in 0..REPOINT_ATTEMPTS {
        let (report, out) = one_pass(registry, &mut remap)?;
        let Some(out) = out else {
            return Ok(report);
        };
        let Some(fresh) = read_registry(registry)? else {
            return Err(Error::Io {
                context: format!("{}: deleted under the pass; refusing to resurrect it", registry.display()),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "the registry vanished after the pass read it"),
            });
        };
        if fresh == out.read_bytes {
            atomic_write(registry, out.text.as_bytes())?;
            return Ok(report);
        }
    }
    Err(Error::Io {
        context: format!("{}: changed under the pass after {REPOINT_ATTEMPTS} attempts", registry.display()),
        source: std::io::Error::new(std::io::ErrorKind::WouldBlock, "the registry keeps changing"),
    })
}

/// One read → scan → remap → build pass. `None` means nothing to write; the
/// first pass's raw bytes ride the output so the caller can compare them
/// against a fresh read before committing the rename.
fn one_pass(registry: &Path, remap: &mut impl FnMut(&str) -> Remap) -> Result<(RepointReport, Option<PassOut>)> {
    let Some(bytes) = read_registry(registry)? else {
        return Ok((RepointReport::default(), None));
    };
    let text = std::str::from_utf8(&bytes).map_err(|source| Error::Io {
        context: format!("reading {}: not utf-8", registry.display()),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
    })?;

    // One pass collects the decisions, a second builds the output, so the
    // report's order is the file's and nothing is appended for a no-change run.
    let values = quoted_values(text);
    let mut report = RepointReport::default();
    let mut decisions = Vec::with_capacity(values.len());
    for (open, close) in values {
        let value = &text[open + 1..close];
        let decision = match remap(value) {
            Remap::Keep => Decision::Keep,
            Remap::Rewrite(to) if to == value => Decision::Keep,
            Remap::Rewrite(to) => {
                if !report.rewritten.iter().any(|r| r.from == value) {
                    report.rewritten.push(Repointed { from: value.to_string(), to: to.clone() });
                }
                Decision::Rewrite(to)
            }
            Remap::Skip(reason) => {
                if !report.skipped.iter().any(|s| s.path == value) {
                    report.skipped.push(RepointSkip { path: value.to_string(), reason });
                }
                Decision::Keep
            }
        };
        decisions.push((open, close, decision));
    }
    if !report.changed() {
        return Ok((report, None));
    }

    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (open, close, decision) in decisions {
        out.push_str(&text[last..open]);
        match decision {
            Decision::Keep => out.push_str(&text[open..=close]),
            Decision::Rewrite(to) => {
                out.push('"');
                out.push_str(&to);
                out.push('"');
            }
        }
        last = close + 1;
    }
    out.push_str(&text[last..]);
    Ok((report, Some(PassOut { text: out, read_bytes: bytes })))
}

/// What one pass read, beside the output built from it. The bytes are the
/// drift witness: the caller compares them against a fresh read so a
/// concurrent write between the two reads restarts the pass instead of being
/// dropped by the rename.
struct PassOut {
    text: String,
    read_bytes: Vec<u8>,
}

enum Decision {
    Keep,
    Rewrite(String),
}

/// Read the registry, mapping absence to `None` and any other failure to the
/// crate error.
fn read_registry(registry: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(registry) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io { context: format!("reading {}", registry.display()), source: e }),
    }
}

/// The byte ranges of every quoted value's interior, opening quote to closing
/// quote: `(open, close)` indexes into `text`. An unterminated final string is
/// dropped. `\` escapes the following byte, so an escaped quote never reads as
/// the terminator, matching JSON's own rule for the shapes a registry holds.
/// Values and keys scan alike; the remap owns the distinction.
fn quoted_values(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'"' {
            i += 1;
            continue;
        }
        let open = i;
        i += 1;
        while i < b.len() && b[i] != b'"' {
            i = if b[i] == b'\\' { i + 2 } else { i + 1 };
        }
        if i < b.len() {
            out.push((open, i));
        }
        i += 1;
    }
    out
}

#[cfg(test)]
#[path = "../tests/unit/repoint.rs"]
mod repoint_tests;
