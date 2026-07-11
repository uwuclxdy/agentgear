//! gemini backend unit tests: portability filtering for both mcp servers and
//! hooks, since a `${CLAUDE_PLUGIN_ROOT}`-bearing entry is never written and
//! must therefore never be a candidate for removal either (removing an
//! unfiltered name could delete an unrelated user-owned entry of the same name).

use std::collections::BTreeMap;

use super::{hook_is_portable, portable_names, reconcile_hooks, remove_hooks};
use crate::components::{HookBinding, McpKind, McpServer};

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-gemini-unit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: Vec::new(), env: BTreeMap::new() }
}

#[test]
fn portable_names_excludes_claude_plugin_root_servers() {
    let servers = [server("ez-fixture", "host_fixture"), server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky")];
    assert_eq!(portable_names(&servers), vec!["ez-fixture"]);
}

#[test]
fn hook_portability_matches_mcp_server_rule() {
    let portable = HookBinding { event: "SessionStart".into(), matcher: None, command: "host_fixture self-heal".into() };
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh".into() };
    assert!(hook_is_portable(&portable));
    assert!(!hook_is_portable(&rooted));
}

#[test]
fn reconcile_hooks_skips_non_portable_and_remove_hooks_leaves_a_same_name_survivor() {
    let path = scratch("settings.json");
    let rooted = HookBinding { event: "SessionStart".into(), matcher: None, command: "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh".into() };

    // A non-portable hook is never written.
    let changed = reconcile_hooks(&path, std::slice::from_ref(&rooted)).unwrap();
    assert!(!changed, "a non-portable hook must not be written");
    assert!(!path.exists(), "reconcile_hooks must not create a file for zero writable hooks");

    // Seed the file with a user's own hook whose command happens to equal the
    // literal (unexpanded) non-portable command we would have written — proves
    // `remove_hooks` never touches it, since we never wrote it.
    std::fs::write(&path, format!(r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}"}}]}}]}}}}"#, rooted.command))
        .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    let removed = remove_hooks(&path, std::slice::from_ref(&rooted)).unwrap();
    assert!(!removed, "remove_hooks must not remove a hook it never wrote");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "seeded hook survived byte-for-byte");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
