//! omp backend unit tests: the mcp reconcile/probe/remove path omp drives through
//! the shared json renderer (`mcpServers`, Plain shape) — exact written shape, a
//! second-reconcile `NoOp`, exact removal that preserves a pre-seeded user entry,
//! and probe classification — plus the translation-specific bits (portability
//! filtering, plugin-prefixed flat file names, the agent re-emit).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{doc_file, flat_stem, portable_names, render_agent};
use crate::agents::BackendState;
use crate::agents::mcpjson::{self, ServerShape};
use crate::components::{MarkdownDoc, McpKind, McpServer};
use crate::host::Outcome;

const KEY: &[&str] = &["mcpServers"];

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-omp-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str, args: &[&str]) -> McpServer {
    McpServer {
        name: name.into(),
        kind: McpKind::Stdio,
        command: command.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        env: BTreeMap::new(),
    }
}

fn agent_doc(name: &str, rel: &str, extra: &[(&str, &str)], body: &str) -> MarkdownDoc {
    let mut frontmatter = BTreeMap::new();
    frontmatter.insert("name".into(), Value::from(name));
    for (k, v) in extra {
        frontmatter.insert((*k).into(), Value::from(*v));
    }
    MarkdownDoc { name: name.into(), rel: rel.into(), frontmatter, body: body.into(), raw: body.as_bytes().to_vec() }
}

#[test]
fn portable_names_excludes_claude_plugin_root_servers() {
    let servers = [server("ez-fixture", "host_fixture", &["mcp"]), server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[])];
    assert_eq!(portable_names(&servers), vec!["ez-fixture"]);
}

#[test]
fn doc_file_flattens_and_prefixes() {
    assert_eq!(doc_file("ez-fixture-plugin", "commands/hello.md", "commands/"), "ez-fixture-plugin-hello.md");
    assert_eq!(doc_file("ez-fixture-plugin", "commands/sub/nested.md", "commands/"), "ez-fixture-plugin-sub-nested.md");
    assert_eq!(doc_file("ez-fixture-plugin", "agents/ez-helper.md", "agents/"), "ez-fixture-plugin-ez-helper.md");
    // No `.md` suffix must still drop the stripped prefix, not reintroduce the subdir.
    assert_eq!(flat_stem("commands/hello", "commands/"), "hello");
}

#[test]
fn render_agent_re_emits_name_and_description_and_drops_model() {
    let doc = agent_doc(
        "ez-helper",
        "agents/ez-helper.md",
        &[("description", "fixture subagent so the plugin exercises an agent-def component"), ("model", "sonnet")],
        "A fixture helper agent body that becomes systemPrompt.",
    );
    let out = render_agent("ez-fixture-plugin", &doc);
    assert!(out.starts_with("---\n"), "frontmatter fence missing:\n{out}");
    // Name is namespaced off the file stem (ownership + omp's exact-name dedup), so a
    // bare `ez-helper` can never collide with a bundled/user agent of the same name.
    assert!(out.contains("name: ez-fixture-plugin-ez-helper\n"), "namespaced name missing:\n{out}");
    assert!(out.contains("description: fixture subagent so the plugin exercises an agent-def component\n"), "description missing:\n{out}");
    // The CC `model` alias is dropped (not an omp model id).
    assert!(!out.contains("model") && !out.contains("sonnet"), "CC model alias leaked:\n{out}");
    // The markdown body is preserved verbatim as the omp systemPrompt.
    assert!(out.contains("A fixture helper agent body that becomes systemPrompt."), "body missing:\n{out}");
}

#[test]
fn render_agent_quotes_a_colon_bearing_description() {
    let doc = agent_doc("x", "agents/x.md", &[("description", "does: things")], "body");
    assert!(render_agent("p", &doc).contains("description: \"does: things\"\n"));
}

#[test]
fn mcp_reconcile_writes_plain_shape_then_no_ops_and_preserves_a_user_entry() {
    let path = scratch("mcp.json");
    // A user's own server + an unrelated top-level key that must survive our merge.
    std::fs::write(&path, r#"{"$schema":"https://x","mcpServers":{"theirs":{"command":"their-server","args":[]}}}"#).unwrap();

    let servers = [server("ez-fixture", "host_fixture", &["mcp"])];
    assert_eq!(mcpjson::reconcile(&path, KEY, &servers, ServerShape::Plain).unwrap(), Outcome::Installed);

    let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    // Exact Plain body: `{command,args,env}`, no `type` (omp defaults stdio).
    assert_eq!(root["mcpServers"]["ez-fixture"], json!({ "command": "host_fixture", "args": ["mcp"], "env": {} }));
    // The seeded user server + unrelated key survived untouched.
    assert_eq!(root["mcpServers"]["theirs"], json!({ "command": "their-server", "args": [] }));
    assert_eq!(root["$schema"], json!("https://x"));

    // A second identical reconcile is a true NoOp (no write).
    assert_eq!(mcpjson::reconcile(&path, KEY, &servers, ServerShape::Plain).unwrap(), Outcome::NoOp);

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mcp_probe_classifies_absent_healthy_and_needs_repair() {
    let path = scratch("mcp.json");
    let servers = [server("ez-fixture", "host_fixture", &["mcp"])];

    // Absent: a file with no `mcpServers` entry of ours.
    std::fs::write(&path, r#"{"mcpServers":{}}"#).unwrap();
    assert!(matches!(mcpjson::probe(&path, KEY, &servers, ServerShape::Plain).unwrap(), BackendState::Absent));

    // Healthy: our exact render present.
    mcpjson::reconcile(&path, KEY, &servers, ServerShape::Plain).unwrap();
    assert!(matches!(mcpjson::probe(&path, KEY, &servers, ServerShape::Plain).unwrap(), BackendState::Healthy));

    // NeedsRepair: present but drifted (a different command under our key).
    std::fs::write(&path, r#"{"mcpServers":{"ez-fixture":{"command":"stale","args":[],"env":{}}}}"#).unwrap();
    assert!(matches!(mcpjson::probe(&path, KEY, &servers, ServerShape::Plain).unwrap(), BackendState::NeedsRepair));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mcp_remove_deletes_only_ours_and_a_non_portable_name_is_never_targeted() {
    let path = scratch("mcp.json");
    // Seed a user server whose name collides with a non-portable server we declare
    // but never write — proves `remove` keys off `portable_names`, not the raw list.
    std::fs::write(&path, r#"{"mcpServers":{"theirs":{"command":"their-server","args":[]},"rooted":{"command":"user-owned"}}}"#).unwrap();
    let servers = [server("ez-fixture", "host_fixture", &["mcp"]), server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky", &[])];

    // reconcile writes only the portable server; the non-portable one is skipped.
    mcpjson::reconcile(&path, KEY, &servers, ServerShape::Plain).unwrap();
    let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(root["mcpServers"].get("ez-fixture").is_some(), "portable server must be written");
    assert_eq!(root["mcpServers"]["rooted"], json!({ "command": "user-owned" }), "non-portable name must not overwrite the user's entry");

    // remove drops only our portable key; the user's `theirs` + same-named `rooted` survive.
    assert_eq!(mcpjson::remove(&path, KEY, &portable_names(&servers)).unwrap(), Outcome::Removed);
    let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(root["mcpServers"].get("ez-fixture").is_none(), "our server survived remove");
    assert_eq!(root["mcpServers"]["theirs"], json!({ "command": "their-server", "args": [] }));
    assert_eq!(root["mcpServers"]["rooted"], json!({ "command": "user-owned" }), "remove wrongly touched a same-named user entry");
    // Nothing of ours left -> probe reports Absent.
    assert!(matches!(mcpjson::probe(&path, KEY, &servers, ServerShape::Plain).unwrap(), BackendState::Absent));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
