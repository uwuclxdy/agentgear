//! Shared skills-dir surface for the non-CC backends that host a loose
//! `<name>/SKILL.md` skill root. Renders the plugin's `SkillDir` IR to disk with an
//! ownership sentinel injected into each SKILL.md's frontmatter, so `remove` and
//! `probe` only ever touch skills we authored. The `~/.agents/skills/` root is read
//! by ~10 tools (and is zed's ONLY skill path), so a skill the user or another tool
//! dropped there must never be swept — the tag, not a dir prefix, carries ownership.
//!
//! Skill names stay bare (`<name>/SKILL.md`, no plugin prefix) so `/skill:<name>`
//! works as the CC author wrote it; two plugins colliding on a same-named skill in a
//! shared root is an accepted v1 edge.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use super::BackendState;
use super::confedit::{write_file_idem, yaml_scalar};
use super::report;
use crate::components::SkillDir;
use crate::error::{Error, IoContext, Result};
use crate::host::{Plugin, Scope};

/// Frontmatter key naming the owning `<plugin>@<marketplace>`. Presence marks a
/// SKILL.md as ours; the value scopes ownership so two agentgear hosts sharing one
/// skills root never delete each other's skills.
const TAG_KEY: &str = "x-agentgear";

/// The canonical CC skill entry file, relative to the skill dir.
const SKILL_MD: &str = "SKILL.md";

/// The cross-tool `~/.agents/skills` (user) / `<project>/.agents/skills` (project)
/// root: zed's only skill path and one of devin's native scan roots. HOME-based at
/// user scope (real `$HOME`, honored by every tool reading this convention), so a
/// test redirecting `HOME` redirects it.
pub(crate) fn agents_skills_root(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => dirs::home_dir()
            .map(|h| h.join(".agents").join("skills"))
            .ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.agents/skills".into())),
        Scope::Project { path } => Ok(path.join(".agents").join("skills")),
    }
}

/// Write every skill's files under `root/<name>/`, injecting our ownership tag into
/// each SKILL.md. Returns whether anything changed (drives NoOp vs Installed). A
/// second reconcile is a true `NoOp`: the render is deterministic from the source.
/// A skill slot already occupied by a foreign SKILL.md (present but not ours) is
/// skipped WHOLE — neither its SKILL.md nor its support files are written — so the
/// shared root is never clobbered, matching `remove`/`probe`'s ownership gate.
pub(crate) fn reconcile(root: &Path, plugin: &Plugin, skills: &[SkillDir]) -> Result<bool> {
    if skills.is_empty() {
        return Ok(false);
    }
    let tag = plugin.id();
    let mut changed = false;
    for skill in skills {
        // Never overwrite a foreign same-named skill (would clobber its bytes AND stamp
        // our tag, then a later uninstall would sweep it). An absent or ours slot writes.
        if slot_ownership(&root.join(&skill.name), &tag)? == Some(false) {
            continue;
        }
        for (path, bytes) in skill_files(root, &tag, skill) {
            changed |= write_file_idem(&path, &bytes)?;
        }
    }
    Ok(changed)
}

/// Delete each `root/<name>/` skill dir whose SKILL.md carries our tag. A same-named
/// skill the user or another tool authored (no tag, or a different owner) is left
/// untouched — the shared root is never swept of a foreign skill.
pub(crate) fn remove(root: &Path, plugin: &Plugin, skills: &[SkillDir]) -> Result<bool> {
    let tag = plugin.id();
    let mut changed = false;
    for skill in skills {
        let dir = root.join(&skill.name);
        if slot_ownership(&dir, &tag)? == Some(true) {
            fs::remove_dir_all(&dir).io_ctx(|| format!("removing {}", dir.display()))?;
            changed = true;
        }
    }
    Ok(changed)
}

/// Classify the skills surface for `probe`: compare each skill's rendered SKILL.md to
/// disk, counting only tagged files as ours. `None` when the plugin declares no skill
/// (nothing to own). SKILL.md is the ownership + drift anchor: a missing one reads
/// `Absent`, a tagged-but-drifted one `NeedsRepair`, a foreign (untagged) replacement
/// is left alone (contributes nothing). Reuses [`report::probe_files`].
pub(crate) fn probe(root: &Path, plugin: &Plugin, skills: &[SkillDir]) -> Result<Option<BackendState>> {
    if skills.is_empty() {
        return Ok(None);
    }
    let tag = plugin.id();
    let expected: Vec<(PathBuf, Vec<u8>)> =
        skills.iter().map(|s| (root.join(&s.name).join(SKILL_MD), inject(&tag, &s.name, skill_md_source(s)))).collect();
    // Ownership scoped to the frontmatter tag (not a whole-file substring): a foreign
    // SKILL.md whose body merely mentions the tag line is not misread as our drift.
    report::probe_files(&expected, |_, existing| std::str::from_utf8(existing).is_ok_and(|t| frontmatter_has_tag(t, &tag)))
}

// --- rendering ---------------------------------------------------------------

/// The `(abs path, rendered bytes)` files for one skill dir rooted at `root/<name>/`.
/// SKILL.md gets our tag injected; every other file copies through verbatim. A skill
/// with no SKILL.md gets a synthesized one so the dir always carries our tag.
fn skill_files(root: &Path, tag: &str, skill: &SkillDir) -> Vec<(PathBuf, Vec<u8>)> {
    let dir = root.join(&skill.name);
    let mut out = Vec::with_capacity(skill.files.len() + 1);
    let mut wrote_skill_md = false;
    for (rel, bytes) in &skill.files {
        if is_skill_md(rel) {
            wrote_skill_md = true;
            out.push((dir.join(rel), inject(tag, &skill.name, bytes)));
        } else {
            out.push((dir.join(rel), bytes.clone()));
        }
    }
    if !wrote_skill_md {
        out.push((dir.join(SKILL_MD), inject(tag, &skill.name, b"")));
    }
    out
}

/// The source SKILL.md bytes for a skill (empty when it ships none, so a synthesized
/// SKILL.md still gets a tag). Both `probe` and `reconcile` render from this, so their
/// bytes are identical.
fn skill_md_source(skill: &SkillDir) -> &[u8] {
    skill.files.iter().find(|(rel, _)| is_skill_md(rel)).map(|(_, b)| b.as_slice()).unwrap_or(b"")
}

/// Inject the ownership tag into a SKILL.md's YAML frontmatter, ensuring `name` and
/// `description` exist (the harnesses that require them, e.g. copilot-cli/zed). Source
/// frontmatter lines are preserved verbatim; missing keys are appended. Deterministic
/// from the source so a re-reconcile is a byte-identical NoOp.
fn inject(tag: &str, name: &str, source: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(source);
    let (fm, body) = split_frontmatter(&text);
    let has = |key: &str| fm.iter().any(|l| top_key(l) == Some(key));

    let mut out = String::from("---\n");
    for line in &fm {
        // Drop a pre-existing tag so re-injecting ours stays idempotent.
        if top_key(line) == Some(TAG_KEY) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !has("name") {
        let _ = writeln!(out, "name: {}", yaml_scalar(name));
    }
    if !has("description") {
        let _ = writeln!(out, "description: {}", yaml_scalar(name));
    }
    out.push_str(&tag_line(tag));
    out.push_str("\n---\n");
    out.push_str(&body);
    out.into_bytes()
}

fn tag_line(tag: &str) -> String {
    format!("{TAG_KEY}: {}", yaml_scalar(tag))
}

fn is_skill_md(rel: &str) -> bool {
    rel == SKILL_MD
}

/// The top-level key of a frontmatter line (`key:` at column 0), or `None` for a
/// blank, indented, or continuation line.
fn top_key(line: &str) -> Option<&str> {
    if line.starts_with([' ', '\t']) {
        return None;
    }
    let key = line.split_once(':')?.0.trim();
    (!key.is_empty()).then_some(key)
}

/// Ownership of an on-disk skill slot's SKILL.md: `None` when absent (writable),
/// `Some(true)` ours, `Some(false)` foreign. Used by both `reconcile` (skip a foreign
/// slot) and `remove` (delete only ours) so the two can never disagree.
fn slot_ownership(dir: &Path, tag: &str) -> Result<Option<bool>> {
    let skill_md = dir.join(SKILL_MD);
    match fs::read_to_string(&skill_md) {
        Ok(text) => Ok(Some(frontmatter_has_tag(&text, tag))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io { context: format!("reading {}", skill_md.display()), source }),
    }
}

/// Whether a SKILL.md's FRONTMATTER carries our `x-agentgear` key with our exact
/// value. Scoped to the frontmatter block, not a whole-file substring: a foreign
/// skill whose body happens to contain the tag line is not misread as ours.
fn frontmatter_has_tag(text: &str, tag: &str) -> bool {
    let (fm, _) = split_frontmatter(text);
    let want = yaml_scalar(tag);
    fm.iter().any(|line| top_key(line) == Some(TAG_KEY) && line.split_once(':').map(|(_, v)| v.trim()) == Some(want.as_str()))
}

/// Split a leading `---`-fenced YAML block into (raw inner lines, body). No fence ->
/// (empty, whole text). Line-based: it preserves each inner line verbatim (so a flat
/// `key: value` block round-trips untouched) but does not fully parse YAML. The
/// terminator is an exact column-0 `---` fence, so an indented `---` (e.g. a markdown
/// divider inside a `|` block-scalar value) is NOT mistaken for the closing fence.
fn split_frontmatter(text: &str) -> (Vec<String>, String) {
    let rest = match text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) {
        Some(rest) => rest,
        None => return (Vec::new(), text.to_string()),
    };
    let mut lines = Vec::new();
    let mut pos = 0usize;
    while pos < rest.len() {
        let nl = rest[pos..].find('\n').map(|i| pos + i);
        let line = rest[pos..nl.unwrap_or(rest.len())].trim_end_matches('\r');
        let next = nl.map_or(rest.len(), |i| i + 1);
        // Column-0 `---` only: an indented `---` stays inside the frontmatter body.
        if line == "---" {
            return (lines, rest.get(next..).unwrap_or_default().to_string());
        }
        lines.push(line.to_string());
        pos = next;
    }
    // Unterminated fence: treat the whole thing as body, no frontmatter.
    (Vec::new(), text.to_string())
}

#[cfg(test)]
#[path = "../../tests/unit/skillsdir.rs"]
mod skillsdir_tests;
