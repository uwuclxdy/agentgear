//! Build-time portability lint: `non_portable_warning` must name every entry whose
//! command or args carry `${CLAUDE_PLUGIN_ROOT}` and stay silent when all are portable.

use std::collections::BTreeMap;

use super::*;
use crate::components::{HookBinding, McpKind, McpServer, PluginComponents};

fn server(name: &str, command: &str, args: &[&str]) -> McpServer {
    McpServer {
        name: name.to_string(),
        kind: McpKind::Stdio,
        command: command.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        env: BTreeMap::new(),
    }
}

fn hook(event: &str, command: &str) -> HookBinding {
    HookBinding { event: event.to_string(), matcher: None, command: command.to_string() }
}

#[test]
fn silent_when_every_entry_is_portable() {
    let components = PluginComponents {
        mcp_servers: vec![server("bare", "node", &["server.js"])],
        hooks: vec![hook("SessionStart", "mytool self-heal")],
        ..PluginComponents::default()
    };
    assert_eq!(non_portable_warning(&components), None);
}

#[test]
fn names_the_non_portable_entries_only() {
    let components = PluginComponents {
        // The token can ride the command or an arg; the canonical plugin shape puts
        // it in args (`node ${CLAUDE_PLUGIN_ROOT}/server.js`).
        mcp_servers: vec![server("clean", "node", &["server.js"]), server("rooted", "node", &["${CLAUDE_PLUGIN_ROOT}/server.js"])],
        hooks: vec![hook("SessionStart", "mytool self-heal"), hook("PreToolUse", "${CLAUDE_PLUGIN_ROOT}/hook.sh")],
        ..PluginComponents::default()
    };
    let warning = non_portable_warning(&components).expect("must warn when an entry is non-portable");

    assert!(warning.contains("mcp server `rooted`"), "names the non-portable server: {warning}");
    assert!(warning.contains("`PreToolUse` hook"), "names the non-portable hook: {warning}");
    assert!(!warning.contains("clean"), "must not name the portable server: {warning}");
    assert!(!warning.contains("SessionStart"), "must not name the portable hook: {warning}");
    // Explains the token and stays one line so cargo emits a single warning.
    assert!(warning.contains("${CLAUDE_PLUGIN_ROOT}"), "explains the token: {warning}");
    assert!(!warning.contains('\n'), "single line for cargo:warning: {warning}");
}
