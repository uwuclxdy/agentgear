//! `skillsdir` render tests: the ownership-tag injection and file layout every
//! skills-capable backend rests on. Pins that the tag lands, `name`/`description`
//! are ensured for the strict harnesses, source frontmatter survives, and the render
//! is idempotent (a re-inject of our own output is a byte-for-byte NoOp).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use super::{frontmatter_has_tag, inject, skill_files, split_frontmatter, tag_line, top_key};
use crate::components::SkillDir;

const TAG: &str = "demo-plugin@demo-mkt";

fn injected(source: &str) -> String {
    String::from_utf8(inject(TAG, "demo", source.as_bytes())).unwrap()
}

#[test]
fn tag_line_quotes_the_at_scoped_id() {
    // `@` is a YAML reserved indicator, so the value must be quoted; the exact line is
    // what `remove`/`probe` match on, so pin it.
    assert_eq!(tag_line(TAG), "x-agentgear: \"demo-plugin@demo-mkt\"");
}

#[test]
fn preserves_existing_frontmatter_and_appends_only_the_tag() {
    let out = injected("---\nname: real-name\ndescription: a real skill\n---\n\n# body\ntext\n");
    // author's keys survive verbatim, not overwritten with the dir name
    assert!(out.contains("name: real-name"), "author name clobbered:\n{out}");
    assert!(out.contains("description: a real skill"), "author description clobbered:\n{out}");
    assert!(out.contains("x-agentgear: \"demo-plugin@demo-mkt\""), "tag missing:\n{out}");
    // exactly one name key (we did not append a second)
    assert_eq!(out.matches("name:").count(), 1, "duplicate name key:\n{out}");
    assert!(out.contains("# body\ntext"), "body dropped:\n{out}");
}

#[test]
fn synthesizes_name_and_description_when_the_source_has_no_frontmatter() {
    let out = injected("# just a body, no frontmatter\n");
    assert!(out.starts_with("---\n"), "no frontmatter fence synthesized:\n{out}");
    assert!(out.contains("name: demo"), "name not synthesized from the dir name:\n{out}");
    assert!(out.contains("description: demo"), "description not synthesized:\n{out}");
    assert!(out.contains("x-agentgear: \"demo-plugin@demo-mkt\""), "tag missing:\n{out}");
    assert!(out.contains("# just a body, no frontmatter"), "body dropped:\n{out}");
}

#[test]
fn reinjecting_our_own_output_is_a_byte_identical_noop() {
    // reconcile renders from the source every time; feeding our tagged output back must
    // not drift (drop-and-re-add the tag, never a second name/description) or the second
    // reconcile would churn instead of NoOp-ing.
    let once = injected("---\nname: real-name\ndescription: d\n---\nbody\n");
    let twice = String::from_utf8(inject(TAG, "demo", once.as_bytes())).unwrap();
    assert_eq!(once, twice, "re-inject drifted");
}

#[test]
fn synthesizes_a_tagged_skill_md_when_the_skill_ships_none() {
    // A skill dir with only support files still gets an owned SKILL.md so `remove`/`probe`
    // have an ownership anchor.
    let skill = SkillDir { name: "demo".into(), files: vec![("assets/note.txt".into(), b"hi".to_vec())] };
    let files = skill_files(Path::new("/root"), TAG, &skill);
    let paths: Vec<String> = files.iter().map(|(p, _)| p.display().to_string()).collect();
    assert!(paths.iter().any(|p| p.ends_with("demo/SKILL.md")), "no synthesized SKILL.md: {paths:?}");
    assert!(paths.iter().any(|p| p.ends_with("demo/assets/note.txt")), "support file dropped: {paths:?}");
    let skill_md = files.iter().find(|(p, _)| p.ends_with("SKILL.md")).map(|(_, b)| b.clone()).unwrap();
    assert!(String::from_utf8(skill_md).unwrap().contains("x-agentgear"), "synthesized SKILL.md lacks the tag");
}

#[test]
fn support_files_copy_through_verbatim() {
    let skill = SkillDir {
        name: "demo".into(),
        files: vec![("SKILL.md".into(), b"---\nname: demo\ndescription: d\n---\nbody\n".to_vec()), ("ref.md".into(), b"raw \x00 bytes".to_vec())],
    };
    let files = skill_files(Path::new("/root"), TAG, &skill);
    let refc = files.iter().find(|(p, _)| p.ends_with("ref.md")).map(|(_, b)| b.clone()).unwrap();
    assert_eq!(refc, b"raw \x00 bytes", "support file was transformed");
}

#[test]
fn split_frontmatter_handles_crlf_and_missing_fence() {
    let (lines, body) = split_frontmatter("---\r\nname: x\r\n---\r\nbody\r\n");
    assert_eq!(lines, vec!["name: x".to_string()]);
    assert_eq!(body, "body\r\n");
    let (none, whole) = split_frontmatter("no fence here\n");
    assert!(none.is_empty());
    assert_eq!(whole, "no fence here\n");
}

#[test]
fn frontmatter_tag_is_scoped_to_the_frontmatter_block() {
    // ours: the tag lives in the frontmatter with our exact value.
    assert!(frontmatter_has_tag("---\nname: x\nx-agentgear: \"demo-plugin@demo-mkt\"\n---\nbody\n", TAG));
    // NOT ours: the tag line appears only in the BODY (a false whole-file substring match).
    assert!(
        !frontmatter_has_tag("---\nname: x\n---\nsee x-agentgear: \"demo-plugin@demo-mkt\" below\n", TAG),
        "a body mention of the tag line must not read as ours"
    );
    // NOT ours: a different plugin's tag value.
    assert!(!frontmatter_has_tag("---\nx-agentgear: \"rival@rival\"\n---\nb\n", TAG), "a rival tag value must not read as ours");
    // NOT ours: no frontmatter at all.
    assert!(!frontmatter_has_tag("plain markdown, no fence\n", TAG));
}

#[test]
fn split_frontmatter_does_not_close_on_an_indented_divider() {
    // A `|` block-scalar description holding a markdown `---` divider (indented) must not
    // be mistaken for the closing fence; only the column-0 `---` terminates.
    let src = "---\ndescription: |\n  intro\n  ---\n  more\nname: real\n---\nreal body\n";
    let (lines, body) = split_frontmatter(src);
    assert!(lines.contains(&"  ---".to_string()), "the indented divider was swallowed as a fence: {lines:?}");
    assert!(lines.contains(&"name: real".to_string()), "frontmatter truncated at the indented divider: {lines:?}");
    assert_eq!(body, "real body\n", "body sliced at the wrong fence: {body:?}");
}

#[test]
fn top_key_ignores_indented_and_blank_lines() {
    assert_eq!(top_key("name: x"), Some("name"));
    assert_eq!(top_key("  nested: y"), None);
    assert_eq!(top_key(""), None);
    assert_eq!(top_key("- listitem"), None); // no colon -> not a key line
}
