//! Components parser tests: mcp (bare-binary + typed remote + `${CLAUDE_PLUGIN_ROOT}`
//! verbatim), referenced/root `.mcp.json`, hook flatten, command/agent frontmatter
//! split, skills grouping. Built from inline `(rel, bytes)` entries, no fixture tree.

use super::*;

fn e(rel: &str, body: &str) -> (String, Vec<u8>) {
    (rel.to_string(), body.as_bytes().to_vec())
}

fn parse(entries: &[(String, Vec<u8>)]) -> PluginComponents {
    PluginComponents::parse(entries).unwrap()
}

#[test]
fn parses_mcp_servers_from_plugin_json() {
    let entries = vec![e(
        ".claude-plugin/plugin.json",
        r#"{
          "name":"p","version":"0.1.0","author":{"name":"a"},
          "mcpServers":{
            "bare":{"command":"host_bin","args":["mcp"],"env":{"K":"V"}},
            "remote":{"type":"http","url":"https://x/mcp"},
            "rooted":{"command":"${CLAUDE_PLUGIN_ROOT}/bin/x"}
          }
        }"#,
    )];
    let c = parse(&entries);
    let by = |n: &str| c.mcp_servers.iter().find(|s| s.name == n).unwrap().clone();

    let bare = by("bare");
    assert_eq!(bare.kind, McpKind::Stdio);
    assert_eq!(bare.command, "host_bin");
    assert_eq!(bare.args, vec!["mcp".to_string()]);
    assert_eq!(bare.env.get("K").map(String::as_str), Some("V"));

    assert_eq!(by("remote").kind, McpKind::Http { url: "https://x/mcp".into() });
    // `${CLAUDE_PLUGIN_ROOT}` is recorded verbatim, never substituted.
    assert_eq!(by("rooted").command, "${CLAUDE_PLUGIN_ROOT}/bin/x");
}

#[test]
fn reads_referenced_and_root_mcp_json() {
    let entries = vec![
        e(".claude-plugin/plugin.json", r#"{"name":"p","mcpServers":".claude-plugin/servers.json"}"#),
        e(".claude-plugin/servers.json", r#"{"mcpServers":{"ref":{"command":"r"}}}"#),
        e(".mcp.json", r#"{"mcpServers":{"root":{"command":"x"}}}"#),
    ];
    let names: Vec<String> = parse(&entries).mcp_servers.into_iter().map(|s| s.name).collect();
    assert!(names.contains(&"ref".to_string()), "referenced .mcp.json server missing: {names:?}");
    assert!(names.contains(&"root".to_string()), "root .mcp.json server missing: {names:?}");
}

#[test]
fn flattens_hooks() {
    let entries = vec![e(
        "hooks/hooks.json",
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"host self-heal"}]}],
                     "UserPromptSubmit":[{"matcher":"*","hooks":[{"type":"command","command":"host check"}]}]}}"#,
    )];
    let c = parse(&entries);
    assert_eq!(c.hooks.len(), 2);
    let ss = c.hooks.iter().find(|h| h.event == "SessionStart").unwrap();
    assert_eq!(ss.command, "host self-heal");
    let up = c.hooks.iter().find(|h| h.event == "UserPromptSubmit").unwrap();
    assert_eq!(up.matcher.as_deref(), Some("*"));
}

#[test]
fn splits_command_and_agent_frontmatter() {
    let entries = vec![e("commands/hi.md", "---\ndescription: say hi\n---\n\nbody here\n"), e("agents/helper.md", "no frontmatter body\n")];
    let c = parse(&entries);
    let cmd = &c.commands[0];
    assert_eq!(cmd.name, "hi");
    assert_eq!(cmd.rel, "commands/hi.md");
    assert_eq!(cmd.frontmatter.get("description").and_then(|v| v.as_str()), Some("say hi"));
    assert_eq!(cmd.body.trim(), "body here");

    let agent = &c.agents[0];
    assert_eq!(agent.name, "helper");
    assert!(agent.frontmatter.is_empty());
    assert_eq!(agent.body.trim(), "no frontmatter body");
    assert_eq!(agent.raw, b"no frontmatter body\n");
}

#[test]
fn block_scalar_frontmatter_value_keeps_its_indented_lines() {
    // A literal block scalar (`description: |`) must join its indented
    // continuation lines, not store the bare `|` indicator and drop them.
    let entries = vec![e("commands/hi.md", "---\ndescription: |\n  first line\n  second line\nother: plain\n---\n\nbody\n")];
    let c = parse(&entries);
    let cmd = &c.commands[0];
    let description = cmd.frontmatter.get("description").and_then(|v| v.as_str()).unwrap_or_default();
    assert_ne!(description, "|", "block scalar indicator stored verbatim instead of its content");
    assert!(description.contains("first line"), "block scalar lost its first line: {description:?}");
    assert!(description.contains("second line"), "block scalar lost its second line: {description:?}");
    assert_eq!(cmd.frontmatter.get("other").and_then(|v| v.as_str()), Some("plain"), "flat key: value parsing regressed");
    assert_eq!(cmd.body.trim(), "body");
}

#[test]
fn crlf_frontmatter_body_is_clean() {
    // A `\r\n`-authored doc must not leak the closing fence's bytes into the body
    // (the byte-offset walk, not a reconstructed line-length sum).
    let entries = vec![e("commands/win.md", "---\r\ndescription: dos\r\n---\r\nreal body line\r\nsecond\r\n")];
    let c = parse(&entries);
    let cmd = &c.commands[0];
    assert_eq!(cmd.frontmatter.get("description").and_then(|v| v.as_str()), Some("dos"));
    assert_eq!(cmd.body, "real body line\r\nsecond\r\n", "body leaked fence bytes: {:?}", cmd.body);
}

#[test]
fn is_portable_flags_plugin_root() {
    let entries = vec![e(
        ".claude-plugin/plugin.json",
        r#"{"name":"p","mcpServers":{"bare":{"command":"host"},"rooted":{"command":"${CLAUDE_PLUGIN_ROOT}/x"}}}"#,
    )];
    let c = parse(&entries);
    assert!(c.mcp_servers.iter().find(|s| s.name == "bare").unwrap().is_portable());
    assert!(!c.mcp_servers.iter().find(|s| s.name == "rooted").unwrap().is_portable());
}

#[test]
fn referenced_mcp_json_may_be_a_bare_map() {
    // A referenced `.mcp.json` without the `mcpServers` wrapper is a bare `{name: spec}` map.
    let entries = vec![
        e(".claude-plugin/plugin.json", r#"{"name":"p","mcpServers":"servers.json"}"#),
        e("servers.json", r#"{"flat":{"command":"f"}}"#),
    ];
    let names: Vec<String> = parse(&entries).mcp_servers.into_iter().map(|s| s.name).collect();
    assert!(names.contains(&"flat".to_string()), "bare-map referenced server missing: {names:?}");
}

#[test]
fn groups_skills_by_top_dir() {
    let entries = vec![e("skills/demo/SKILL.md", "---\nname: demo\n---\nbody"), e("skills/demo/assets/x.txt", "asset")];
    let c = parse(&entries);
    assert_eq!(c.skills.len(), 1);
    let demo = &c.skills[0];
    assert_eq!(demo.name, "demo");
    let files: Vec<&str> = demo.files.iter().map(|(p, _)| p.as_str()).collect();
    assert!(files.contains(&"SKILL.md"), "{files:?}");
    assert!(files.contains(&"assets/x.txt"), "{files:?}");
}
