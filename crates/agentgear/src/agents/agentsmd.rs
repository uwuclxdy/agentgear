//! Shared tagged-block surface for the backends whose harness always-loads a guidance
//! file: either a global context file the user also owns (each harness's own
//! `AGENTS.md`, `GEMINI.md`, `CRUSH.md`, `.goosehints`, …) or a whole plugin-owned file
//! inside a directory the harness scans (cline's `rules/`, kiro's `steering/`, augment's
//! `rules/`) — user scope only. The host's [`Plugin::instructions`] text is rendered
//! between two html-comment markers naming the plugin id, so the id in the markers, not
//! the file, carries ownership: the user's own lines and a rival plugin's block are never
//! touched by a reconcile or a remove, and two agentgear hosts coexist in one file (the
//! same tag-scoped ownership discipline as `skillsdir`).
//!
//! Reconcile appends the block after existing content with one blank separator
//! line, or repairs the content between our markers in place; every byte outside
//! the block survives verbatim (CRLF included), and a converged file is not
//! rewritten. Remove strips the block and the separator line an append added,
//! taking the file only when our own removal emptied it (the accepted cost
//! `confedit`'s `*_remove` root rule already pays). The markers, never the declared
//! text, decide what remove takes, so a host that withdraws its guidance converges to
//! "block gone" instead of stranding the previous text.

use std::path::{Path, PathBuf};

use super::BackendState;
use super::confedit::{remove_file_idem, write_file_idem};
use crate::doctor::{CheckStatus, DoctorCheck};
use crate::error::{Error, Result};
use crate::host::Plugin;

/// Write or repair our guidance block in the `AGENTS.md` at `path`. File absent
/// (or holding nothing but whitespace — there is no user content to preserve)
/// -> the file is created holding only the block, no leading blank line. File
/// present without our markers -> the block is appended after the existing bytes
/// with one blank separator line, terminating the user's last line first when it
/// lacks a newline; existing bytes are never rewritten. Our markers present ->
/// exactly the span between them is replaced by the fresh render. `None`
/// instructions -> no-op. Returns whether anything changed (a converged file
/// writes zero bytes).
pub(crate) fn reconcile(path: &Path, plugin: &Plugin) -> Result<bool> {
    // A host that withdrew its guidance converges to "block gone" instead of stranding
    // the previous text in an always-loaded file no later arm could remove.
    let Some(text) = &plugin.instructions else { return remove(path, plugin) };
    let id = plugin.id();
    let existing = read(path)?;
    let new = match existing.as_deref() {
        None => render_block(&id, text),
        Some(bytes) if bytes.is_empty() || bytes.iter().all(u8::is_ascii_whitespace) => render_block(&id, text),
        Some(bytes) => match block_span(bytes, &id) {
            Some((start, end)) => {
                let mut out = Vec::with_capacity(bytes.len());
                out.extend_from_slice(&bytes[..start]);
                out.extend_from_slice(&render_block(&id, text));
                out.extend_from_slice(&bytes[end..]);
                out
            }
            None => {
                let mut out = Vec::with_capacity(bytes.len());
                out.extend_from_slice(bytes);
                if !out.ends_with(b"\n") {
                    out.push(b'\n');
                }
                out.push(b'\n');
                out.extend_from_slice(&render_block(&id, text));
                out
            }
        },
    };
    write_file_idem(path, &new)
}

/// Strip our block (markers inclusive) plus the one blank separator line an
/// append added; every other byte stays identical. The file goes only when our
/// own removal emptied it (an empty or whitespace-only remainder). No file, no
/// our markers -> no-op. The markers decide, never `Plugin::instructions`: a
/// host that withdrew its guidance still has the block taken back.
pub(crate) fn remove(path: &Path, plugin: &Plugin) -> Result<bool> {
    let id = plugin.id();
    let Some(bytes) = read(path)? else { return Ok(false) };
    let Some((start, end)) = block_span(&bytes, &id) else { return Ok(false) };
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..start]);
    // Strip a BLANK line above the block (two consecutive `\n`), never a bare
    // line terminator: a user line ending directly above a hand-placed block
    // keeps its terminator. CRLF terminates too (`\r\n` + our `\n` ends `\n\n`).
    if out.ends_with(b"\n\n") {
        out.pop();
    }
    out.extend_from_slice(&bytes[end..]);
    if out.is_empty() || out.iter().all(u8::is_ascii_whitespace) {
        remove_file_idem(path)?;
        return Ok(true);
    }
    write_file_idem(path, &out)
}

/// Classify the instructions surface for `probe`: `None` when the plugin declares
/// no guidance (contributes nothing to the composite); `Absent` when the file or
/// our markers are missing, so a hand-deleted block self-heals back in;
/// `Healthy` when the block byte-matches the render; `NeedsRepair` when it
/// drifted. Keys off the same [`block_span`] + [`render_block`] reconcile uses,
/// so probe never demands bytes reconcile did not write.
pub(crate) fn probe(path: &Path, plugin: &Plugin) -> Result<Option<BackendState>> {
    let id = plugin.id();
    let bytes = read(path)?;
    let Some(text) = &plugin.instructions else {
        // Nothing declared: a leftover span is stale state the next heal must clear,
        // and no span means this surface contributes nothing.
        let stranded = bytes.as_deref().is_some_and(|b| block_span(b, &id).is_some());
        return Ok(stranded.then_some(BackendState::NeedsRepair));
    };
    let Some(bytes) = bytes else { return Ok(Some(BackendState::Absent)) };
    let Some((start, end)) = block_span(&bytes, &id) else { return Ok(Some(BackendState::Absent)) };
    let expected = render_block(&id, text);
    Ok(Some(if bytes[start..end] == expected { BackendState::Healthy } else { BackendState::NeedsRepair }))
}

/// The doctor check for this surface: `guidance block present`, naming the
/// AGENTS.md path. Keyed on [`probe`], not bare file existence — the file is
/// shared with the user, so a present file without our block is a failure.
/// `path` is the backend's own user-scope AGENTS.md resolution; `Ok(None)`
/// (a scope with no instructions surface) reads as a warn, never a fail.
pub(crate) fn guidance_check(path: Result<Option<PathBuf>>, plugin: &Plugin) -> DoctorCheck {
    const NAME: &str = "guidance block present";
    match path {
        Ok(Some(file)) => match probe(&file, plugin) {
            Ok(Some(BackendState::Healthy)) => {
                DoctorCheck { name: NAME, status: CheckStatus::Ok(format!("block present in {}", file.display())) }
            }
            Ok(_) => DoctorCheck {
                name: NAME,
                status: CheckStatus::Fail {
                    problem: format!("our guidance block missing or drifted in {}", file.display()),
                    fix: "run the host's `setup`".into(),
                },
            },
            Err(e) => DoctorCheck { name: NAME, status: CheckStatus::Warn(e.to_string()) },
        },
        Ok(None) => DoctorCheck { name: NAME, status: CheckStatus::Warn("no AGENTS.md guidance surface at this scope".into()) },
        Err(e) => DoctorCheck { name: NAME, status: CheckStatus::Warn(e.to_string()) },
    }
}

// --- rendering ---------------------------------------------------------------

/// The `<!-- agentgear:<id> begin|end -->` marker bytes this plugin owns.
fn marker(id: &str, which: &str) -> String {
    format!("<!-- agentgear:{id} {which} -->")
}

/// Our block's exact bytes: the begin marker, the guidance text with a trailing
/// newline guaranteed so the end marker sits on its own line, then the end
/// marker and its line terminator. The one render both reconcile and probe key
/// off, so the two cannot drift.
fn render_block(id: &str, text: &str) -> Vec<u8> {
    let mut out = format!("{}\n", marker(id, "begin"));
    out.push_str(text);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&marker(id, "end"));
    out.push('\n');
    out.into_bytes()
}

/// Where our block lives in `bytes`: `(start, end)` covering both markers and
/// the line terminator after the end marker when present, so the span is exactly
/// the region reconcile owns and may rewrite. `None` when our markers are not in
/// the file. Both markers must START a line — a marker quoted mid-line inside
/// user content is not our block (skillsdir's frontmatter-scoping rule).
fn block_span(bytes: &[u8], id: &str) -> Option<(usize, usize)> {
    let begin = marker(id, "begin").into_bytes();
    let end = marker(id, "end").into_bytes();
    let start = find_line_start(bytes, &begin, 0)?;
    let end_at = find_line_start(bytes, &end, start + begin.len())?;
    let mut span_end = end_at + end.len();
    match bytes.get(span_end..span_end + 2) {
        Some(b"\r\n") => span_end += 2,
        _ if bytes.get(span_end) == Some(&b'\n') => span_end += 1,
        _ => {}
    }
    Some((start, span_end))
}

/// The first index at or after `from` where `needle` occurs and starts a line
/// (index 0 or right after a `\n`). Byte-wise, so non-UTF-8 user content is
/// handled too.
fn find_line_start(bytes: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.len() > bytes.len() {
        return None;
    }
    (from..=bytes.len() - needle.len()).find(|&i| (i == 0 || bytes[i - 1] == b'\n') && &bytes[i..i + needle.len()] == needle)
}

/// The file's bytes, `None` when absent. Raw bytes, never a lossy string, so
/// arbitrary user content round-trips byte-identical through a repair.
fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io { context: format!("reading {}", path.display()), source }),
    }
}

#[cfg(test)]
#[path = "../../tests/unit/agentsmd.rs"]
mod agentsmd_tests;
