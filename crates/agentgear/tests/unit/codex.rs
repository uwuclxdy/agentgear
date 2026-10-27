//! codex backend unit tests: portability filtering for mcp servers and hooks
//! (never exercised by the hermetic fixture e2e test — the fixture plugin has no
//! `${CLAUDE_PLUGIN_ROOT}`-bearing entry), plus the two rendering edge cases
//! caught in review: `flat_stem`'s fallback and DEL-byte TOML escaping.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{
    agent_path, check_hooks_present, hook_is_portable, prompt_path, reconcile_hooks, remove_hooks, render_agent_toml, toml_basic_string,
};
use crate::components::{HookBinding, MarkdownDoc, McpKind, McpServer, portable_names};
use crate::host::Outcome;

fn scratch(name: &str) -> PathBuf {
    let dir = crate::scratch::path("ez-codex-unit");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: Vec::new(), env: BTreeMap::new() }
}

fn doc(rel: &str) -> MarkdownDoc {
    let basename = rel.rsplit('/').next().unwrap_or(rel);
    let name = basename.strip_suffix(".md").unwrap_or(basename);
    MarkdownDoc { name: name.into(), rel: rel.into(), frontmatter: BTreeMap::new(), body: String::new(), raw: Vec::new() }
}

#[test]
fn portable_names_excludes_claude_plugin_root_servers() {
    let servers = [server("ez-fixture", "host_fixture"), server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky")];
    assert_eq!(portable_names(&servers), vec!["ez-fixture"]);
}

#[test]
fn non_portable_server_never_reaches_config_toml() {
    use crate::agents::mcptoml;

    let path = scratch("config.toml");
    let servers = [server("ez-fixture", "host_fixture"), server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky")];

    assert_eq!(mcptoml::reconcile(&path, &servers, true).unwrap(), Outcome::Installed);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[mcp_servers.ez-fixture]"), "portable server must be written:\n{text}");
    assert!(!text.contains("rooted"), "non-portable server leaked into config.toml:\n{text}");

    // remove must key off the same filtered set, never touching a same-named
    // user entry it never wrote (there is none here, but this proves the two
    // call sites — reconcile's writer and remove's deleter — stay in lockstep).
    // Ours was the whole file here, so the file goes with the `[mcp_servers]` table
    // rather than staying behind as the 0 bytes an emptied implicit table renders to.
    assert_eq!(mcptoml::remove(&path, &portable_names(&servers)).unwrap(), Outcome::Removed);
    assert!(!path.exists(), "a config.toml holding nothing but our server must go with it");
}

#[test]
fn hook_portability_matches_mcp_server_rule() {
    let portable = HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() };
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh".into() };
    assert!(hook_is_portable(&portable));
    assert!(!hook_is_portable(&rooted));
}

#[test]
fn reconcile_hooks_skips_non_portable_and_remove_hooks_leaves_a_same_command_survivor() {
    let path = scratch("hooks.json");
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh".into() };

    let changed = reconcile_hooks(&path, std::slice::from_ref(&rooted)).unwrap();
    assert!(!changed, "a non-portable hook must not be written");
    assert!(!path.exists(), "reconcile_hooks must not create a file for zero writable hooks");

    // Seed a user hook whose command happens to equal the literal (unexpanded)
    // non-portable command we would have written — proves `remove_hooks` never
    // touches it, since we never wrote it.
    std::fs::write(&path, format!(r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}"}}]}}]}}}}"#, rooted.command))
        .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    let removed = remove_hooks(&path, std::slice::from_ref(&rooted)).unwrap();
    assert!(!removed, "remove_hooks must not remove a hook it never wrote");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "seeded hook survived byte-for-byte");
}

#[test]
fn nested_doc_paths_flatten_and_keep_the_plugin_prefix() {
    let command = doc("commands/sub/nested.md");
    assert_eq!(
        prompt_path(std::path::Path::new("d"), "ez-fixture-plugin", &command),
        std::path::Path::new("d/ez-fixture-plugin-sub-nested.md")
    );
    let agent = doc("agents/sub/nested.md");
    assert_eq!(
        agent_path(std::path::Path::new("d"), "ez-fixture-plugin", &agent),
        std::path::Path::new("d/ez-fixture-plugin-sub-nested.toml")
    );
    let rendered = render_agent_toml("ez-fixture-plugin", &agent);
    assert!(rendered.contains("name = \"ez-fixture-plugin-sub-nested\""), "agent handle must be the flattened stem:\n{rendered}");
}

#[test]
fn flat_stem_drops_the_kind_dir_without_an_md_suffix() {
    assert_eq!(doc("commands/hello.md").flat_stem(), "hello");
    assert_eq!(doc("commands/nested/hello.md").flat_stem(), "nested-hello");
    assert_eq!(doc("commands/hello").flat_stem(), "hello", "no .md suffix must still drop the kind dir");
}

#[test]
fn toml_basic_string_escapes_del() {
    // TOML basic strings require every control char but tab to be escaped,
    // including DEL (U+007F) — not just the sub-0x20 C0 range.
    let rendered = toml_basic_string("a\u{7F}b");
    assert_eq!(rendered, "\"a\\u007Fb\"");
    // round-trip through a real TOML parser to prove the escape is valid, not just shaped right.
    let doc: toml_edit::DocumentMut = format!("x = {rendered}").parse().unwrap();
    assert_eq!(doc["x"].as_str().unwrap(), "a\u{7F}b");
}

/// Regression: `probe` must render from the SAME resolved source `reconcile` used,
/// not a hardcoded `Source::Embedded`. self_heal rehydrates a `--path` install's
/// source from its marker (`stamp::source_from_marker`), so a probe keying on the
/// embedded blob would (a) misclassify a healthy `--path` install as perpetual
/// `NeedsRepair` when the tree differs from the blob, and (b) ERROR outright on a
/// zero-embed host (empty blob). This drives a zero-embed plugin (`blob: &[]`) so the
/// old `plugin.components(&Source::Embedded)` fails hard, making the mutation red.
#[test]
fn probe_renders_from_the_resolved_source_not_the_embedded_blob() {
    use crate::agents::{AgentBackend, BackendState};
    use crate::host::{Desired, Plugin, Scope, Source};

    // A minimal multi-surface plugin TREE on disk (the `--path` source).
    let src = crate::scratch::path("ez-codex-probe-src");
    std::fs::create_dir_all(src.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(src.join("hooks")).unwrap();
    std::fs::create_dir_all(src.join("commands")).unwrap();
    std::fs::create_dir_all(src.join("agents")).unwrap();
    std::fs::write(
        src.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"ez-probe-src","version":"0.1.0","mcpServers":{"srv":{"command":"echo","args":["hi"]}}}"#,
    )
    .unwrap();
    std::fs::write(
        src.join("hooks").join("hooks.json"),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"echo up"}]}]}}"#,
    )
    .unwrap();
    std::fs::write(src.join("commands").join("hello.md"), "# Hello\n\nsay hi\n").unwrap();
    std::fs::write(src.join("agents").join("helper.md"), "---\nname: helper\ndescription: helps\n---\n\nbody\n").unwrap();

    // Zero-embed: `blob` is empty, so `Source::Embedded` errors at materialize.
    let plugin =
        Plugin { name: "ez-probe-src", marketplace: "ez-mkt", version: "0.1.0", agents: &["codex"], instructions: None, blob: &[] };
    let project = crate::scratch::path("ez-codex-probe-dst");
    let scope = Scope::Project { path: project.clone() };
    let source = Source::Path(src.clone());

    // Converge every surface from the path tree.
    super::CodexBackend.reconcile(&plugin, &Desired { source: source.clone(), reenable: true }, &scope).unwrap();

    // Probe against the SAME resolved source must read Healthy (no churn). The old
    // `Source::Embedded` probe would error here (empty blob) or, with a divergent
    // tree, spuriously read NeedsRepair.
    let state = super::CodexBackend.probe(&plugin, &scope, &source).unwrap();
    assert!(matches!(state, BackendState::Healthy), "a path-sourced install must probe Healthy, not churn");

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn a_dropped_hook_warns_in_the_report() {
    // The hook family's wiring: every declared hook carries the token, so codex
    // writes nothing and the check must say so instead of reading `Ok`.
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/heal.sh".into() };
    let check = check_hooks_present(&[rooted], std::path::Path::new("/nonexistent/hooks.json"));
    let crate::doctor::CheckStatus::Warn(detail) = &check.status else {
        panic!("a dropped hook must warn, got {:?}", check.status);
    };
    assert!(detail.contains("skipped SessionStart: ${CLAUDE_PLUGIN_ROOT}"), "{detail}");
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

    let backend = super::CodexBackend;
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

    let plugin = Plugin { name: "ez-cid", marketplace: "ez-mkt", version: "0.1.0", agents: &["codex"], instructions: None, blob: &[] };
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
