//! cursor backend unit tests: the hook event map. cursor renames CC events to its
//! own camelCase lifecycle names (`SessionStart` -> `sessionStart`, ...). This pins
//! that `SubagentStart` maps to cursor's real `subagentStart` agent hook (present in
//! the binary-confirmed 21-event catalog), mirroring `SubagentStop`.

use super::*;
use crate::components::HookBinding;

/// cursor hosts a `subagentStart` agent hook and already maps
/// `SubagentStop -> subagentStop`, so the CC `SubagentStart` event must map to
/// `subagentStart` too. Guards the `map_event` arm end-to-end through `reconcile_hooks`:
/// without it a CC plugin's `SubagentStart` hook is dropped even though cursor runs it.
#[test]
fn subagent_start_maps_to_cursors_camelcase_event() {
    assert_eq!(map_event("SubagentStart"), Some("subagentStart"), "SubagentStart must map to cursor's subagentStart");

    let dir = crate::scratch::path("ez-cursor-subagentstart");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("hooks.json");
    let sub = HookBinding { event: "SubagentStart".into(), matcher: None, command: "host_fixture note".into() };

    assert!(reconcile_hooks(&path, std::slice::from_ref(&sub)).unwrap(), "SubagentStart hook must be written");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["hooks"]["subagentStart"][0]["command"], "host_fixture note", "SubagentStart must land under hooks.subagentStart:\n{v}");

    std::fs::remove_dir_all(&dir).ok();
}

// --- ${AGENTGEAR_CLIENT} portability token -----------------------------------

/// Recursively true if any file under `root` has contents containing `needle`.
fn agentgear_token_dir_contains(root: &std::path::Path, needle: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(root) else { return false };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if agentgear_token_dir_contains(&p, needle) {
                return true;
            }
        } else if std::fs::read_to_string(&p).is_ok_and(|s| s.contains(needle)) {
            return true;
        }
    }
    false
}

/// A hook command AND an mcp arg both carry `${AGENTGEAR_CLIENT}`; after reconcile
/// each must read this backend's own id, reconcile↔probe must stay Healthy (no
/// perpetual NeedsRepair from a probe that forgot to substitute), and remove must
/// strip the substituted entry it wrote.
#[test]
fn agentgear_client_token_expands_to_this_backend_id() {
    use crate::agents::{AgentBackend, BackendState};
    use crate::host::{Desired, Plugin, Scope, Source};

    let backend = super::CursorBackend;
    let id = backend.id();

    let src = crate::scratch::path("ez-cidtok-src");
    std::fs::create_dir_all(src.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(src.join("hooks")).unwrap();
    std::fs::write(
        src.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"ez-cid","version":"0.1.0","mcpServers":{"srv":{"command":"host_fixture","args":["cid=${AGENTGEAR_CLIENT}"]}}}"#,
    )
    .unwrap();
    std::fs::write(
        src.join("hooks").join("hooks.json"),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"host_fixture up --client ${AGENTGEAR_CLIENT}"}]}]}}"#,
    )
    .unwrap();

    let plugin = Plugin { name: "ez-cid", marketplace: "ez-mkt", version: "0.1.0", agents: &["cursor"], instructions: None, blob: &[] };
    let project = crate::scratch::path("ez-cidtok-dst");
    let scope = Scope::Project { path: project.clone() };
    let source = Source::Path(src.clone());

    backend.reconcile(&plugin, &Desired { source: source.clone(), reenable: true }, &scope).unwrap();

    assert!(agentgear_token_dir_contains(&project, &format!("cid={id}")), "the mcp arg token did not expand to `{id}`");
    assert!(!agentgear_token_dir_contains(&project, "${AGENTGEAR_CLIENT}"), "a raw ${{AGENTGEAR_CLIENT}} token leaked to disk");

    assert!(
        matches!(backend.probe(&plugin, &scope, &source).unwrap(), BackendState::Healthy),
        "a token-bearing install must probe Healthy, not churn"
    );

    backend.remove(&plugin, &scope, &source).unwrap();
    assert!(!agentgear_token_dir_contains(&project, &format!("cid={id}")), "remove left the substituted mcp entry behind");

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&project);
}

// --- instructions (always-apply rule) -----------------------------------------

fn instr_plugin() -> Plugin {
    Plugin {
        name: "ez-cid",
        marketplace: "ez-mkt",
        version: "0.1.0",
        agents: &["cursor"],
        instructions: Some("line one\n".into()),
        blob: &[],
    }
}

#[test]
fn rule_tag_line_quotes_the_at_scoped_id() {
    // `@` is a YAML reserved indicator, so the value must be quoted; the exact line
    // is what probe's ownership filter matches on, so pin it.
    let bytes = render_instructions(&instr_plugin(), "line one\n");
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.starts_with("---\nalwaysApply: true\nx-agentgear: \"ez-cid@ez-mkt\"\n---\n"), "frontmatter:\n{text}");
}

#[test]
fn render_guarantees_exactly_one_trailing_newline() {
    let text = String::from_utf8(render_instructions(&instr_plugin(), "line one")).unwrap();
    assert!(text.ends_with("\n") && !text.ends_with("\n\n"), "render:\n{text}");
}

#[test]
fn frontmatter_tag_is_scoped_to_the_frontmatter_block() {
    let tag = "ez-cid@ez-mkt";
    // ours: the tag lives in the frontmatter with our exact quoted value.
    assert!(frontmatter_has_tag("---\nalwaysApply: true\nx-agentgear: \"ez-cid@ez-mkt\"\n---\nbody\n", tag));
    // NOT ours: the tag line appears only in the BODY (a false whole-file substring match).
    assert!(
        !frontmatter_has_tag("---\nalwaysApply: true\n---\nsee x-agentgear: \"ez-cid@ez-mkt\" below\n", tag),
        "a body mention of the tag line must not read as ours"
    );
    // NOT ours: an indented line is a nested mapping, not a top-level key.
    assert!(
        !frontmatter_has_tag("---\nalwaysApply: true\n  x-agentgear: \"ez-cid@ez-mkt\"\n---\nbody\n", tag),
        "an indented tag line must not read as ours"
    );
    // NOT ours: an unquoted value is not the form our render writes.
    assert!(
        !frontmatter_has_tag("---\nx-agentgear: ez-cid@ez-mkt\n---\nbody\n", tag),
        "a hand-written unquoted tag value must not read as ours"
    );
}

#[test]
fn unterminated_frontmatter_reads_foreign() {
    // An unterminated fence has no frontmatter block at all; the safe direction is
    // foreign (skip-write, skip-delete), never ours.
    assert!(!frontmatter_has_tag("---\nx-agentgear: \"ez-cid@ez-mkt\"\nbody never fenced\n", "ez-cid@ez-mkt"));
}

#[test]
fn probe_treats_a_foreign_slot_as_contributing_nothing() {
    let base = crate::scratch::path("ez-cursor-instr-probe");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("rules")).unwrap();
    let plugin = instr_plugin();

    // absent -> Some(Absent): a deleted rule self-heals back in.
    assert!(matches!(probe_instructions(&base, &plugin).unwrap(), Some(BackendState::Absent)));

    // ours, byte-exact -> Healthy.
    std::fs::write(instructions_path(&base, &plugin), render_instructions(&plugin, "line one\n")).unwrap();
    assert!(matches!(probe_instructions(&base, &plugin).unwrap(), Some(BackendState::Healthy)));

    // ours, drifted -> NeedsRepair.
    std::fs::write(instructions_path(&base, &plugin), "---\nalwaysApply: true\nx-agentgear: \"ez-cid@ez-mkt\"\n---\ndrifted\n").unwrap();
    assert!(matches!(probe_instructions(&base, &plugin).unwrap(), Some(BackendState::NeedsRepair)));

    // foreign (untagged same-named file) -> Ok(None): the slot is the user's and
    // contributes nothing to the composite, so a foreign occupant never churns.
    std::fs::write(instructions_path(&base, &plugin), "---\nalwaysApply: true\n---\ntheir body\n").unwrap();
    assert!(probe_instructions(&base, &plugin).unwrap().is_none());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn instructions_check_classifies_by_probe_not_file_existence() {
    let base = crate::scratch::path("ez-cursor-instr-check");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("rules")).unwrap();
    let plugin = instr_plugin();

    // absent -> fail naming the fix (a present-but-foreign file must NOT fail below).
    assert!(matches!(instructions_check(&base, &plugin).status, CheckStatus::Fail { .. }));

    std::fs::write(instructions_path(&base, &plugin), render_instructions(&plugin, "line one\n")).unwrap();
    assert!(matches!(instructions_check(&base, &plugin).status, CheckStatus::Ok(_)));

    std::fs::write(instructions_path(&base, &plugin), "---\nalwaysApply: true\n---\ntheir body\n").unwrap();
    assert!(
        matches!(instructions_check(&base, &plugin).status, CheckStatus::Warn(_)),
        "a foreign slot is the user's own file: warn, never fail"
    );

    let _ = std::fs::remove_dir_all(&base);
}
