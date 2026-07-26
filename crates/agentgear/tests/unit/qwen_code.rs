//! qwen-code backend unit tests: the portability filter (a `${CLAUDE_PLUGIN_ROOT}`
//! entry is never written, so it must never be a removal candidate either), the CC
//! nested hook shape + identity event mapping, the mcp Plain shape + probe
//! classification, and the agent/command namespacing helpers. The full lifecycle is
//! covered end-to-end by `crates/host-fixture/tests/qwen_code.rs`.

use std::collections::BTreeMap;
use std::fs;

use super::{agent_file, command_rel, hook_is_portable, reconcile_hooks, remove_hooks, render_agent};
use crate::agents::BackendState;
use crate::agents::mcpjson::{self, ServerShape};
use crate::components::{HookBinding, MarkdownDoc, McpKind, McpServer};
use crate::host::Outcome;
use serde_json::Value;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = crate::scratch::path("ez-qwen-unit");
    fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: Vec::new(), env: BTreeMap::new() }
}

fn doc(rel: &str) -> MarkdownDoc {
    MarkdownDoc { name: "x".into(), rel: rel.into(), frontmatter: BTreeMap::new(), body: String::new(), raw: Vec::new() }
}

#[test]
fn hook_portability_matches_mcp_server_rule() {
    let portable = HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() };
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh".into() };
    assert!(hook_is_portable(&portable));
    assert!(!hook_is_portable(&rooted));
}

#[test]
fn reconcile_hooks_writes_cc_nested_shape_with_identity_events_and_is_idempotent() {
    let path = scratch("settings.json");
    let start = HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() };
    // A matcher'd tool event proves the identity map + matcher preservation.
    let pre = HookBinding { event: "PreToolUse".into(), matcher: Some("Bash".into()), command: "host_fixture guard".into() };

    let changed = reconcile_hooks(&path, &[start.clone(), pre.clone()]).unwrap();
    assert!(changed, "first reconcile must write");

    let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    // SessionStart maps identically; CC's nested `{hooks:[{type:"command",command}]}` shape.
    let group = &v["hooks"]["SessionStart"][0];
    assert_eq!(group["hooks"][0]["type"], "command");
    assert_eq!(group["hooks"][0]["command"], "host_fixture self-heal");
    // PreToolUse maps identically and keeps the matcher.
    let pre_group = &v["hooks"]["PreToolUse"][0];
    assert_eq!(pre_group["matcher"], "Bash");
    assert_eq!(pre_group["hooks"][0]["command"], "host_fixture guard");

    // A second identical reconcile writes nothing.
    assert!(!reconcile_hooks(&path, &[start, pre]).unwrap(), "second reconcile must be a no-op");

    fs::remove_dir_all(path.parent().unwrap()).ok();
}

/// `SubagentStart` is a CC hook event and qwen-code's own `HookEventName` enum
/// contains it verbatim (verify-qwen-code #6), so the identity map must carry it
/// through like every other shared event. Guards the `map_event` arm: without it, a
/// CC plugin's `SubagentStart` hook is silently dropped even though qwen runs it.
#[test]
fn subagent_start_maps_identically() {
    let path = scratch("settings.json");
    let sub = HookBinding { event: "SubagentStart".into(), matcher: None, command: "host_fixture note".into() };

    assert!(reconcile_hooks(&path, std::slice::from_ref(&sub)).unwrap(), "SubagentStart hook must be written");
    let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        v["hooks"]["SubagentStart"][0]["hooks"][0]["command"], "host_fixture note",
        "SubagentStart must map 1:1 and land under hooks.SubagentStart:\n{v}"
    );

    fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn reconcile_hooks_skips_non_portable_and_remove_leaves_a_same_name_survivor() {
    let path = scratch("settings.json");
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh".into() };

    // A non-portable hook is never written (and no empty file is created).
    assert!(!reconcile_hooks(&path, std::slice::from_ref(&rooted)).unwrap());
    assert!(!path.exists(), "reconcile_hooks must not create a file for zero writable hooks");

    // Seed a user hook whose command equals the literal (unexpanded) command we
    // would have written — proves `remove_hooks` never touches a hook we never wrote.
    fs::write(&path, format!(r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}"}}]}}]}}}}"#, rooted.command))
        .unwrap();
    let before = fs::read_to_string(&path).unwrap();
    assert!(!remove_hooks(&path, std::slice::from_ref(&rooted)).unwrap(), "remove must not touch a hook we never wrote");
    assert_eq!(fs::read_to_string(&path).unwrap(), before, "seeded hook survived byte-for-byte");

    fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mcp_reconcile_writes_plain_shape_and_probe_classifies_lifecycle() {
    let path = scratch("settings.json");
    let servers = [server("ez-fixture", "host_fixture")];

    // A missing settings file classifies Absent (nothing of ours present).
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), BackendState::Absent));

    // Reconcile writes the qwen Plain shape: `{command,args,env}`, no `type` field
    // (qwen picks the transport by which key is present).
    assert_eq!(mcpjson::reconcile(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), Outcome::Installed);
    let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let entry = &v["mcpServers"]["ez-fixture"];
    assert_eq!(entry["command"], "host_fixture");
    assert!(entry.get("type").is_none(), "a qwen stdio server must carry no `type` field: {entry}");
    assert!(entry.get("args").is_some() && entry.get("env").is_some());

    // Now Healthy, and a second reconcile is a true NoOp.
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), BackendState::Healthy));
    assert_eq!(mcpjson::reconcile(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), Outcome::NoOp);

    // A drifted body (same key, different command) classifies NeedsRepair.
    let drifted = [server("ez-fixture", "some-other-cmd")];
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &drifted, ServerShape::plain()).unwrap(), BackendState::NeedsRepair));

    fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mcp_non_portable_is_skipped_and_probe_stays_healthy() {
    let path = scratch("settings.json");
    fs::write(&path, "{}").unwrap();
    let rooted = server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/x");

    // Reconcile never writes the non-portable server key (the shared renderer may
    // ensure an empty `mcpServers` object, which is benign).
    mcpjson::reconcile(&path, &["mcpServers"], std::slice::from_ref(&rooted), ServerShape::plain()).unwrap();
    let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(v["mcpServers"].get("rooted").is_none(), "a non-portable server must never be written:\n{v}");
    // On an existing file, probe returns Healthy (never Absent) so self_heal never
    // drops a present marker for a plugin with no portable servers.
    assert!(matches!(
        mcpjson::probe(&path, &["mcpServers"], std::slice::from_ref(&rooted), ServerShape::plain()).unwrap(),
        BackendState::Healthy
    ));
    // A second reconcile is a true NoOp.
    assert_eq!(mcpjson::reconcile(&path, &["mcpServers"], std::slice::from_ref(&rooted), ServerShape::plain()).unwrap(), Outcome::NoOp);

    fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mcp_remove_deletes_only_ours_and_keeps_a_user_entry() {
    let path = scratch("settings.json");
    fs::write(&path, r#"{"mcpServers":{"ez-fixture":{"command":"host_fixture","args":["mcp"]},"theirs":{"command":"their-server"}}}"#)
        .unwrap();
    // A non-portable server we declared but never wrote must not become a removal
    // target, even if a user has a server of the same name.
    let declared = [server("ez-fixture", "host_fixture"), server("rooted", "${CLAUDE_PLUGIN_ROOT}/x")];

    assert_eq!(mcpjson::remove(&path, &["mcpServers"], &declared, ServerShape::plain()).unwrap(), Outcome::Removed);
    let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(v["mcpServers"].get("ez-fixture").is_none(), "our server should be gone");
    assert!(v["mcpServers"].get("theirs").is_some(), "the user's server must survive");

    fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn render_agent_namespaces_name_and_drops_model() {
    let mut fm = BTreeMap::new();
    fm.insert("name".to_string(), Value::from("ez-helper"));
    fm.insert("description".to_string(), Value::from("a fixture helper"));
    fm.insert("model".to_string(), Value::from("sonnet"));
    let agent = MarkdownDoc {
        name: "ez-helper".into(),
        rel: "agents/ez-helper.md".into(),
        frontmatter: fm,
        body: "  Body text.  ".into(),
        raw: Vec::new(),
    };

    let out = render_agent("ez-fixture-plugin", &agent);
    // Name is JSON-quoted (a valid YAML flow scalar), same escaping as description.
    assert!(out.contains(r#"name: "ez-fixture-plugin-ez-helper""#), "name must be plugin-prefixed and quoted:\n{out}");
    assert!(out.contains("description:"), "description must carry over:\n{out}");
    assert!(!out.contains("model:") && !out.contains("sonnet"), "the CC model alias must be dropped:\n{out}");
    assert!(out.ends_with("Body text.\n"), "body copies through, trimmed:\n{out}");
}

#[test]
fn render_agent_quotes_yaml_special_name() {
    // A name carrying YAML indicators (`: ` and a leading `@`) must not leak raw into
    // the frontmatter, where it would break qwen's YAML parser. JSON-quoting wraps it
    // in a single double-quoted flow scalar (old raw-emit produced `name: weird: @a`).
    let mut fm = BTreeMap::new();
    fm.insert("name".to_string(), Value::from("weird: @agent"));
    let agent = MarkdownDoc { name: "weird".into(), rel: "agents/weird.md".into(), frontmatter: fm, body: "Body.".into(), raw: Vec::new() };

    let out = render_agent("plug", &agent);
    assert!(out.contains("\nname: \"plug-weird: @agent\"\n"), "a YAML-special name must be double-quoted:\n{out}");
    // A quote in the name is escaped, still a well-formed double-quoted scalar.
    let mut fm2 = BTreeMap::new();
    fm2.insert("name".to_string(), Value::from(r#"a"b"#));
    let agent2 = MarkdownDoc { name: "a".into(), rel: "agents/a.md".into(), frontmatter: fm2, body: String::new(), raw: Vec::new() };
    assert!(render_agent("p", &agent2).contains("\nname: \"p-a\\\"b\"\n"), "an embedded quote must be escaped");
}

#[test]
fn command_and_agent_path_helpers() {
    // Commands keep their subdir (qwen's `:` namespacing); copy-through preserves `.md`.
    assert_eq!(command_rel(&doc("commands/hello.md")), "hello.md");
    assert_eq!(command_rel(&doc("commands/git/commit.md")), "git/commit.md");
    // Agents flatten to a plugin-prefixed flat file.
    assert_eq!(agent_file("ez-fixture-plugin", &doc("agents/ez-helper.md")), "ez-fixture-plugin-ez-helper.md");
    assert_eq!(agent_file("ez-fixture-plugin", &doc("agents/deep/nested.md")), "ez-fixture-plugin-deep-nested.md");
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

    let backend = super::QwenCodeBackend;
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

    let plugin = Plugin {
        name: "ez-cid",
        marketplace: "ez-mkt",
        version: "0.1.0",
        agents: &["qwen-code"],
        instructions: None,
        statusline: None,
        blob: &[],
    };
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
