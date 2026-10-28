//! kimi backend unit tests: the bespoke `[[hooks]]` toml path (exact shape,
//! idempotency, exact removal under a colliding event) plus the portability filter
//! for both mcp servers and hooks — a `${CLAUDE_PLUGIN_ROOT}`-bearing entry is never
//! written and so must never be a removal candidate either. The mcp side rides the
//! shared `mcpjson` renderer (covered by its own tests + the hermetic e2e), so these
//! focus on what kimi owns.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{
    expected_agents, hook_is_portable, hook_present, map_event, reconcile_agents, reconcile_hooks, remove_agents, remove_hooks,
    rendered_agent,
};
use crate::agents::mcpjson::{self, ServerShape};
use crate::components::{HookBinding, MarkdownDoc, McpKind, McpServer};
use crate::host::Outcome;
use serde_json::Value;

fn scratch(name: &str) -> PathBuf {
    let dir = crate::scratch::path("ez-kimi-unit");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn server(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: Vec::new(), env: BTreeMap::new() }
}

fn hook(event: &str, command: &str) -> HookBinding {
    HookBinding { event: event.into(), matcher: None, command: command.into() }
}

#[test]
fn hook_portability_matches_mcp_server_rule() {
    let portable = hook("SessionStart", "host_fixture self-heal");
    let rooted = hook("SessionStart", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh");
    assert!(hook_is_portable(&portable));
    assert!(!hook_is_portable(&rooted));
}

#[test]
fn map_event_passes_kimi_events_and_skips_the_unknown() {
    // Kimi mirrors CC event names 1:1 for the overlap set.
    assert_eq!(map_event("SessionStart"), Some("SessionStart"));
    assert_eq!(map_event("UserPromptSubmit"), Some("UserPromptSubmit"));
    assert_eq!(map_event("PreToolUse"), Some("PreToolUse"));
    // A name kimi does not define is skipped rather than guessed.
    assert_eq!(map_event("PreToolFictional"), None);
}

#[test]
fn reconcile_hooks_writes_expected_toml_shape_and_second_reconcile_noops() {
    let path = scratch("config.toml");
    let matched = HookBinding { event: "PreToolUse".into(), matcher: Some("Bash".into()), command: "host_fixture guard".into() };
    let hooks = [hook("SessionStart", "host_fixture self-heal"), matched];

    let changed = reconcile_hooks(&path, &hooks).unwrap();
    assert!(changed, "first reconcile must write");
    let text = std::fs::read_to_string(&path).unwrap();

    // Exact `[[hooks]]` array-of-tables shape, event names 1:1, matcher only when set.
    assert!(text.contains("[[hooks]]"), "no array-of-tables emitted:\n{text}");
    assert!(text.contains("event = \"SessionStart\""), "SessionStart event missing:\n{text}");
    assert!(text.contains("command = \"host_fixture self-heal\""), "SessionStart command missing:\n{text}");
    assert!(text.contains("event = \"PreToolUse\""), "PreToolUse event missing:\n{text}");
    assert!(text.contains("matcher = \"Bash\""), "matcher missing for the matched hook:\n{text}");
    assert!(text.contains("command = \"host_fixture guard\""), "PreToolUse command missing:\n{text}");
    // A hook with no matcher must not emit a matcher key.
    assert!(!text.contains("matcher = \"\""), "an empty matcher key leaked in:\n{text}");
    // Round-trips through a real TOML parser (proves valid `[[hooks]]`, not just shaped text).
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(doc["hooks"].as_array_of_tables().unwrap().len(), 2, "expected two hook tables");

    // Idempotent: a converged reconcile touches no bytes.
    let changed = reconcile_hooks(&path, &hooks).unwrap();
    assert!(!changed, "second reconcile must be a NoOp");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "second reconcile changed bytes");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn reconcile_hooks_skips_non_portable_and_remove_hooks_leaves_a_same_command_survivor() {
    let path = scratch("config.toml");
    let rooted = hook("SessionStart", "${CLAUDE_PLUGIN_ROOT}/hooks/self-heal.sh");

    // A non-portable hook is never written (and no empty file is created).
    let changed = reconcile_hooks(&path, std::slice::from_ref(&rooted)).unwrap();
    assert!(!changed, "a non-portable hook must not be written");
    assert!(!path.exists(), "reconcile_hooks must not create a file for zero writable hooks");

    // Seed a user hook whose command equals the literal (unexpanded) non-portable
    // command we would have written — proves `remove_hooks` never touches it.
    let seed = format!("[[hooks]]\nevent = \"SessionStart\"\ncommand = \"{}\"\n", rooted.command);
    std::fs::write(&path, &seed).unwrap();
    let removed = remove_hooks(&path, std::slice::from_ref(&rooted)).unwrap();
    assert!(!removed, "remove_hooks must not remove a hook it never wrote");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), seed, "seeded hook survived byte-for-byte");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn remove_hooks_strips_ours_keeps_user_under_a_colliding_event() {
    let path = scratch("config.toml");
    // A user hook under SessionStart (the same event we write to) with its own command.
    let seed = "[[hooks]]\nevent = \"SessionStart\"\ncommand = \"their-session-hook\"\n";
    std::fs::write(&path, seed).unwrap();

    let ours = hook("SessionStart", "host_fixture self-heal");
    assert!(reconcile_hooks(&path, std::slice::from_ref(&ours)).unwrap(), "our hook should be appended");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("their-session-hook") && text.contains("host_fixture self-heal"), "both hooks should coexist:\n{text}");

    // Remove ours: the user's same-event hook must survive; the file must still parse.
    assert!(remove_hooks(&path, std::slice::from_ref(&ours)).unwrap(), "our hook should be removed");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("host_fixture self-heal"), "our hook survived removal:\n{text}");
    assert!(text.contains("their-session-hook"), "user's same-event hook was removed:\n{text}");
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(doc["hooks"].as_array_of_tables().unwrap().len(), 1, "only the user hook should remain");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn hook_present_matches_on_event_matcher_and_command() {
    let path = scratch("config.toml");
    let matched = HookBinding { event: "PreToolUse".into(), matcher: Some("Bash".into()), command: "host_fixture guard".into() };
    reconcile_hooks(&path, std::slice::from_ref(&matched)).unwrap();
    let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    let arr = doc["hooks"].as_array_of_tables().unwrap();

    assert!(hook_present(arr, "PreToolUse", &matched), "identical entry must count as present");
    // A different matcher is a distinct entry (not the same hook).
    let other_matcher = HookBinding { matcher: Some("Edit".into()), ..matched.clone() };
    assert!(!hook_present(arr, "PreToolUse", &other_matcher), "a differing matcher must not match");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn mcp_probe_classifies_absent_healthy_and_needs_repair() {
    // The kimi backend routes mcp through the shared json renderer, so drive it the
    // same way `reconcile`/`probe` do (mcp.json `mcpServers`, Plain shape).
    let path = scratch("mcp.json");
    let servers = [server("ez-fixture", "host_fixture")];

    // Absent: nothing written yet.
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), crate::agents::BackendState::Absent));

    // Healthy: after a reconcile the on-disk body matches.
    assert_eq!(mcpjson::reconcile(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(), Outcome::Installed);
    assert!(matches!(
        mcpjson::probe(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(),
        crate::agents::BackendState::Healthy
    ));

    // NeedsRepair: the key is present but its body drifted from what we'd render.
    std::fs::write(&path, r#"{"mcpServers":{"ez-fixture":{"command":"tampered","args":[],"env":{}}}}"#).unwrap();
    assert!(matches!(
        mcpjson::probe(&path, &["mcpServers"], &servers, ServerShape::plain()).unwrap(),
        crate::agents::BackendState::NeedsRepair
    ));

    // A plugin with only a non-portable server is Healthy (never Absent), so a
    // present marker is never dropped for an mcp-less-after-filtering plugin.
    let rooted = [server("rooted", "${CLAUDE_PLUGIN_ROOT}/bin/leaky")];
    assert!(matches!(mcpjson::probe(&path, &["mcpServers"], &rooted, ServerShape::plain()).unwrap(), crate::agents::BackendState::Healthy));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
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

    let backend = super::KimiBackend;
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

    let plugin = Plugin { name: "ez-cid", marketplace: "ez-mkt", version: "0.1.0", agents: &["kimi"], instructions: None, blob: &[] };
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

// --- agents (kimi 2.3.3 user agent profiles) --------------------------------

/// A CC agent def as the components IR holds it: frontmatter split out, path kept.
fn agent_doc(rel: &str, frontmatter: Vec<(&str, Value)>, body: &str) -> MarkdownDoc {
    MarkdownDoc {
        name: rel.rsplit('/').next().and_then(|f| f.strip_suffix(".md")).unwrap_or(rel).to_string(),
        rel: rel.to_string(),
        frontmatter: frontmatter.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        body: body.to_string(),
        raw: Vec::new(),
    }
}

#[test]
fn kimi_declares_the_agents_capability() {
    use crate::agents::AgentBackend;

    assert!(super::KimiBackend.capabilities().agents, "kimi 2.3.3 scans `$KIMI_CODE_HOME/agents` for `*.md` agent profiles");
}

#[test]
fn rendered_agent_emits_kimi_frontmatter_with_a_plugin_prefixed_kebab_name() {
    let doc = agent_doc(
        "agents/ez-helper.md",
        vec![
            ("name", Value::String("ez-helper".into())),
            ("description", Value::String("fixture subagent so the plugin exercises an agent-def component".into())),
            ("tools", Value::String("Read, Grep, Glob".into())),
            ("model", Value::String("sonnet".into())),
        ],
        "\nA fixture helper agent.\n",
    );

    let (file, bytes) = rendered_agent("ez-fixture-plugin", &doc).expect("an agent with a description renders");
    assert_eq!(file, "ez-fixture-plugin-ez-helper.md");
    assert_eq!(
        bytes,
        "---\nname: ez-fixture-plugin-ez-helper\ndescription: fixture subagent so the plugin exercises an agent-def component\ntools: \"Read, Grep, Glob\"\n---\n\nA fixture helper agent.\n",
        "kimi requires `description`, takes `tools` as a comma string, and has no agent `model` field"
    );

    // The plugin name is only a single file-name component, not necessarily kebab
    // (a capital, an underscore pass the derive's check), so it is slugged too —
    // otherwise kimi skips every profile the plugin ships.
    let plain = agent_doc("agents/ez-helper.md", vec![("description", Value::String("fixture".into()))], "body\n");
    assert_eq!(
        rendered_agent("Ez_Fixture", &plain).expect("a non-kebab plugin name still renders").1.lines().nth(1),
        Some("name: ez-fixture-ez-helper"),
        "the rendered `name` must satisfy kimi's kebab pattern whatever the plugin is called"
    );

    // A plugin name may legally start with a dot, and kimi's directory walk skips any
    // entry that does — so the FILE name must be slugged too, not just the frontmatter
    // name, or the profile is invisible to kimi while every check stays green.
    assert_eq!(
        rendered_agent(".plugin", &plain).expect("a dot-leading plugin name still renders").0,
        "plugin-ez-helper.md",
        "a leading dot in the plugin name must not reach the file name"
    );
}

#[test]
fn rendered_agent_slugs_a_non_kebab_name_and_falls_back_to_the_path_stem() {
    let from_frontmatter = agent_doc(
        "agents/Reviewer.md",
        vec![("name", Value::String("Code_Reviewer".into())), ("description", Value::String("reviews diffs".into()))],
        "You review.\n",
    );
    assert_eq!(
        rendered_agent("ez-fixture-plugin", &from_frontmatter).expect("a non-kebab name still renders").0,
        "ez-fixture-plugin-code-reviewer.md",
        "kimi skips a file whose `name` is not kebab-case (AGENT_NAME_PATTERN)"
    );

    let from_path = agent_doc("agents/helpers/deep-check.md", vec![("description", Value::String("checks deeply".into()))], "You check.\n");
    assert_eq!(
        rendered_agent("ez-fixture-plugin", &from_path).expect("a nameless agent renders off its stem").0,
        "ez-fixture-plugin-helpers-deep-check.md"
    );
}

#[test]
fn rendered_agent_skips_an_agent_kimi_could_not_load() {
    let no_description = agent_doc("agents/silent.md", vec![("name", Value::String("silent".into()))], "body\n");
    assert!(
        rendered_agent("ez-fixture-plugin", &no_description).is_none(),
        "kimi skips a file without `description`; writing one leaves an agent kimi never loads"
    );

    let blank_description = agent_doc("agents/blank.md", vec![("description", Value::String("   ".into()))], "body\n");
    assert!(rendered_agent("ez-fixture-plugin", &blank_description).is_none(), "a whitespace-only description is not one");

    let empty_body = agent_doc("agents/mute.md", vec![("description", Value::String("no prompt".into()))], "  \n");
    assert!(rendered_agent("ez-fixture-plugin", &empty_body).is_none(), "kimi rejects a profile with no prompt body");

    let unsluggable = agent_doc("agents/___.md", vec![("description", Value::String("no name".into()))], "body\n");
    assert!(
        rendered_agent("ez-fixture-plugin", &unsluggable).is_none(),
        "a name holding no alphanumerics can never satisfy kimi's kebab pattern"
    );
}

/// Two distinct CC agents can slugify to one kimi profile name (`foo-bar`/`foo_bar`),
/// and kimi keys profiles on that name. Rendering both would make `reconcile` alternate
/// between two byte sets (always `Installed`) and `probe` always `NeedsRepair` — a heal
/// that churns every session. One deterministic survivor converges instead, and doctor
/// names the collision rather than reporting the single file as healthy.
#[test]
fn expected_agents_dedupes_a_slug_collision_and_doctor_flags_it() {
    use crate::agents::{BackendState, report};
    use crate::doctor::CheckStatus;

    let root = crate::scratch::path("ez-kimi-collide");
    let agents_dir = root.join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    let docs = [
        agent_doc("agents/foo_bar.md", vec![("description", Value::String("underscore agent".into()))], "underscore body\n"),
        agent_doc("agents/foo-bar.md", vec![("description", Value::String("hyphen agent".into()))], "hyphen body\n"),
    ];
    let expected = expected_agents(&agents_dir, "ez-fixture-plugin", &docs);
    assert_eq!(expected.len(), 1, "a slug collision must render one file, not two: {expected:?}");
    let rendered = String::from_utf8_lossy(&expected[0].1);
    assert!(
        rendered.contains("hyphen agent"),
        "the rel-sorted first doc must win deterministically (`agents/foo-bar.md` sorts before `agents/foo_bar.md`):\n{rendered}"
    );

    assert!(reconcile_agents(&agents_dir, "ez-fixture-plugin", &docs).unwrap(), "first reconcile writes");
    let state = report::probe_files(&expected, |_, _| true).unwrap();
    assert!(matches!(state, Some(BackendState::Healthy)), "a collided set must converge Healthy, not NeedsRepair forever");
    assert!(!reconcile_agents(&agents_dir, "ez-fixture-plugin", &docs).unwrap(), "a collided set must reconcile to NoOp");

    let check = super::check_agents_present(&docs, &agents_dir, "ez-fixture-plugin");
    let problem = match &check.status {
        CheckStatus::Fail { problem, .. } => problem.clone(),
        other => panic!("a slug collision must fail the doctor check, got {other:?}"),
    };
    assert!(
        problem.contains("agents/foo_bar.md") && problem.contains("agents/foo-bar.md"),
        "the failing check must name both colliding agents:\n{problem}"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// A plugin whose agents all lack what kimi requires translates nothing at all. That
/// is a translate limit, not a failure — but the check must say so, never report a
/// cheerful `0 agent file(s) present` under an `[ ok ]`.
#[test]
fn check_agents_present_notes_a_wholly_untranslatable_set() {
    use crate::doctor::CheckStatus;

    let root = crate::scratch::path("ez-kimi-skipnote");
    let agents_dir = root.join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    let docs = [agent_doc("agents/silent.md", vec![], "body\n")];
    let check = super::check_agents_present(&docs, &agents_dir, "ez-fixture-plugin");
    let text = match &check.status {
        CheckStatus::Ok(text) => text.clone(),
        other => panic!("a wholly skipped set is a translate limit, not a failure: {other:?}"),
    };
    assert!(text.contains("skipped"), "the check must name the skipped agent(s), got {text:?}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reconcile_agents_writes_ours_probes_healthy_and_removes_only_ours() {
    use crate::agents::{BackendState, report};

    let root = crate::scratch::path("ez-kimi-agents");
    let agents_dir = root.join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    let foreign = agents_dir.join("theirs.md");
    let foreign_body = "---\ndescription: their own agent\n---\ntheir prompt\n";
    std::fs::write(&foreign, foreign_body).unwrap();

    let docs =
        [agent_doc("agents/ez-helper.md", vec![("description", Value::String("fixture subagent".into()))], "A fixture helper agent.\n")];
    let ours = agents_dir.join("ez-fixture-plugin-ez-helper.md");
    let expected = expected_agents(&agents_dir, "ez-fixture-plugin", &docs);

    assert!(matches!(report::probe_files(&expected, |_, _| true).unwrap(), Some(BackendState::Absent)));
    assert!(reconcile_agents(&agents_dir, "ez-fixture-plugin", &docs).unwrap(), "first reconcile writes");
    assert!(ours.exists(), "our agent file missing: {}", ours.display());
    assert!(matches!(report::probe_files(&expected, |_, _| true).unwrap(), Some(BackendState::Healthy)));
    assert!(!reconcile_agents(&agents_dir, "ez-fixture-plugin", &docs).unwrap(), "second reconcile must no-op");

    assert!(remove_agents(&agents_dir, "ez-fixture-plugin", &docs).unwrap(), "remove takes ours");
    assert!(!ours.exists(), "our agent file survived removal");
    assert_eq!(std::fs::read_to_string(&foreign).unwrap(), foreign_body, "a foreign agent file must survive byte-identically");

    std::fs::remove_dir_all(&root).ok();
}
