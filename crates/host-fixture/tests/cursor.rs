//! Hermetic cursor-backend lifecycle, fully isolated from the real `~/.cursor`.
//! No docker, no auth, no cursor binary: the backend only ever writes cursor's
//! file config, so we drive `host_fixture setup --agent cursor` against a temp
//! `HOME` (+ XDG dirs) and assert the written `mcp.json` / `hooks.json` /
//! `commands` / `agents` by reading them back. `detect()` passes off the
//! pre-created `~/.cursor` dir alone (no `cursor`/`cursor-agent` on PATH).
//!
//! Every path the backend touches derives from `HOME`, which we point at a
//! throwaway temp root — so proving our files land under that root (and the seeded
//! user entries survive) also proves the backend never reaches the real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our
/// install and uninstall untouched.
const SEED_MCP: &str = r#"{
  "editorTelemetry": false,
  "mcpServers": {
    "theirs": { "type": "stdio", "command": "their-server", "args": [] }
  }
}
"#;

/// A foreign hook under an event we also write to (`sessionStart`, collides with
/// our `SessionStart` mapping) plus one under an event we never touch, both of
/// which MUST outlive our install and uninstall. `hooks.json` is a file separate
/// from `mcp.json`, so this seed is load-bearing: without it, hook never-clobber
/// goes completely unverified.
const SEED_HOOKS: &str = r#"{
  "version": 1,
  "hooks": {
    "sessionStart": [
      { "command": "their-session-hook" }
    ],
    "stop": [
      { "command": "their-stop-hook" }
    ]
  }
}
"#;

struct Env {
    root: PathBuf,
    cursor: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("cursor")` /
    /// `which("cursor-agent")` (and every other backend's PATH probe) stays false
    /// and detection rides on `~/.cursor` alone.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-cursor-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            cursor: root.join(".cursor"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create ~/.cursor so detect() passes with no cursor CLI on PATH, and
        // seed an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.cursor).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.cursor.join("mcp.json"), SEED_MCP).unwrap();
        fs::write(env.cursor.join("hooks.json"), SEED_HOOKS).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("PATH", &self.path);
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn mcp(&self) -> String {
        fs::read_to_string(self.cursor.join("mcp.json")).unwrap()
    }

    fn hooks(&self) -> String {
        fs::read_to_string(self.cursor.join("hooks.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `cursor`, no sibling agent
/// CLIs, so the fan-out stays a pure cursor exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

/// Seed Claude Code's on-disk plugin registry under `claude_dir` (a `.../.claude`): the
/// `installed_plugins.json` cursor's default-on `loadClaude` loader reads (numeric
/// `version` key mandatory, same shape omp's `claude-plugins` provider reads — the two
/// backends share `registry_lists_plugin`). The plugin id matches the fixture:
/// `<name>@<marketplace>`, both `ez-fixture-plugin`.
fn seed_cc_registry(claude_dir: &Path) {
    const ID: &str = "ez-fixture-plugin@ez-fixture-plugin";
    let plugins = claude_dir.join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    let install = claude_dir.join("cache").join("ez-fixture-plugin");
    let registry = serde_json::json!({
        "version": 1,
        "plugins": { ID: [{ "scope": "user", "installPath": install.to_string_lossy() }] },
    });
    fs::write(plugins.join("installed_plugins.json"), serde_json::to_vec(&registry).unwrap()).unwrap();
}

#[test]
fn cursor_reconcile_noops_when_cc_registry_covers() {
    // CC's registry lists the plugin, so cursor's own `loadClaude` loader already
    // surfaces the whole plugin tree (mcp/hooks/skills/rules/agents/commands) off it —
    // translating any of it ourselves would double-register every surface. The retire
    // is total (unlike omp's agents-only retire): no cursor file is written at all.
    let env = Env::new("cc-covers");
    seed_cc_registry(&env.root.join(".claude"));

    let (ok, out) = env.fixture(&["setup", "--agent", "cursor"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "NoOp", "a CC-registry-covered install must not translate, got {out}");

    // Nothing of ours landed: the seeded user files are untouched, no new surface dirs.
    assert_eq!(env.mcp(), SEED_MCP, "mcp.json was touched despite CC-registry coverage");
    assert_eq!(env.hooks(), SEED_HOOKS, "hooks.json was touched despite CC-registry coverage");
    assert!(!env.cursor.join("commands").exists(), "commands/ written despite CC-registry coverage");
    assert!(!env.cursor.join("agents").exists(), "agents/ written despite CC-registry coverage");
    assert!(!env.cursor.join("skills").exists(), "skills/ written despite CC-registry coverage");

    // probe reads Healthy (nothing owned, marker kept), not Absent — self_heal must
    // never churn a covered install, and must never treat it as "plugin gone."
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal on a covered install should no-op, got {out}");

    // remove stays unconditional: nothing of ours exists, so uninstall itself no-ops
    // (there is nothing to clean up), but it must not error.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "NoOp", "uninstall on a covered (never-translated) install failed: {out}");
}

#[test]
fn cursor_translates_when_cc_registry_relocated() {
    // A relocated `CLAUDE_CONFIG_DIR` moves CC's real registry off cursor's own
    // HARDCODED `join(HOME, ".claude")` read path (cursor's loader ignores
    // `CLAUDE_CONFIG_DIR` entirely). Our gate
    // reads the same HOME-based path, so it must also miss the relocated registry and
    // fall back to full translation — never silently losing cursor's coverage.
    let env = Env::new("relocated");
    let alt = env.root.join("altcfg").join(".claude");
    seed_cc_registry(&alt);
    assert!(!env.root.join(".claude").exists(), "test setup error: HOME-based .claude must be absent");

    let mut cmd = Command::new(BIN);
    cmd.args(["setup", "--agent", "cursor"]);
    env.apply(&mut cmd);
    cmd.env("CLAUDE_CONFIG_DIR", env.root.join("altcfg"));
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "setup failed: {}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "Installed",
        "a relocated CLAUDE_CONFIG_DIR must not read as CC-registry-covered"
    );

    // Full translation actually landed.
    assert!(env.mcp().contains("ez-fixture"), "mcp not translated under a relocated CLAUDE_CONFIG_DIR");
    assert!(
        env.cursor.join("agents").join("ez-fixture-plugin-ez-helper.md").exists(),
        "agent file not translated under a relocated CLAUDE_CONFIG_DIR"
    );
}

#[test]
fn cursor_full_lifecycle() {
    let env = Env::new("lifecycle");
    let cmd_file = env.cursor.join("commands").join("ez-fixture-plugin-hello.md");
    let agent_file = env.cursor.join("agents").join("ez-fixture-plugin-ez-helper.md");
    let hooks_file = env.cursor.join("hooks.json");
    let skill_dir = env.cursor.join("skills").join("ez-skill");
    let skill = skill_dir.join("SKILL.md");

    // install: translates mcp + hooks + commands + agents into cursor's config.
    let (ok, out) = env.fixture(&["setup", "--agent", "cursor"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // A fresh, healthy install must self-heal to a true NoOp: probe reads every surface
    // reconcile just wrote and finds no drift. Guards against a probe/reconcile desync
    // (widened surface probe, or probe rendering from the wrong source) that would churn.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    // mcp: our server landed under `mcpServers`, Typed shape (explicit stdio type).
    let m = env.mcp();
    assert!(m.contains("ez-fixture"), "our mcp server key missing:\n{m}");
    assert!(m.contains("host_fixture"), "our mcp command missing:\n{m}");
    assert!(m.contains("\"type\": \"stdio\""), "Typed shape (type:stdio) missing:\n{m}");
    // remote mcp: both arms land in cursor's exact accepted shape.
    let parsed: serde_json::Value = serde_json::from_str(&m).unwrap();
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-http"],
        serde_json::json!({"type": "http", "url": "http://127.0.0.1:39621/mcp", "headers": {}}),
        "http remote arm mismatch:\n{m}"
    );
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-sse"],
        serde_json::json!({"type": "sse", "url": "http://127.0.0.1:39622/sse", "headers": {}}),
        "sse remote arm mismatch:\n{m}"
    );
    // the seeded user config survived our merge.
    assert!(m.contains("theirs") && m.contains("their-server"), "seeded mcp server was clobbered:\n{m}");
    assert!(m.contains("editorTelemetry"), "seeded top-level key was clobbered:\n{m}");

    // hooks: SessionStart -> sessionStart, UserPromptSubmit -> beforeSubmitPrompt.
    assert!(hooks_file.exists(), "hooks.json not written: {}", hooks_file.display());
    let h = env.hooks();
    assert!(h.contains("\"version\": 1"), "hooks.json missing version:\n{h}");
    assert!(h.contains("sessionStart"), "sessionStart hook missing:\n{h}");
    assert!(h.contains("beforeSubmitPrompt"), "UserPromptSubmit was not mapped to beforeSubmitPrompt:\n{h}");
    assert!(h.contains("self-heal"), "sessionStart hook command missing:\n{h}");
    assert!(h.contains("check-restart"), "beforeSubmitPrompt hook command missing:\n{h}");
    // the seeded user hooks survived our merge: one under an event we also write
    // to (sessionStart), one under an event we never touch.
    assert!(h.contains("their-session-hook"), "seeded sessionStart hook was clobbered:\n{h}");
    assert!(h.contains("their-stop-hook"), "seeded stop hook was clobbered:\n{h}");

    // commands: one plain-markdown file, no frontmatter leaked into the prompt.
    assert!(cmd_file.exists(), "command file not written: {}", cmd_file.display());
    let c = fs::read_to_string(&cmd_file).unwrap();
    assert!(c.contains("Say hello"), "command body not translated:\n{c}");
    assert!(!c.contains("description:"), "frontmatter leaked into the command prompt:\n{c}");

    // agents: one cursor subagent file, plugin-prefixed name, model coerced.
    assert!(agent_file.exists(), "agent file not written: {}", agent_file.display());
    let a = fs::read_to_string(&agent_file).unwrap();
    assert!(a.contains("name: ez-fixture-plugin-ez-helper"), "agent name not plugin-prefixed:\n{a}");
    assert!(a.contains("model: inherit"), "agent model not coerced to inherit:\n{a}");
    assert!(a.contains("fixture helper agent"), "agent body not translated:\n{a}");

    // skills: bare `<name>/SKILL.md` under ~/.cursor/skills, ownership-tagged, support file copied.
    assert!(skill.exists(), "skill SKILL.md not written: {}", skill.display());
    let sk = fs::read_to_string(&skill).unwrap();
    assert!(sk.contains("name: ez-skill"), "skill name frontmatter missing:\n{sk}");
    assert!(sk.contains("description:"), "skill description frontmatter missing:\n{sk}");
    assert!(sk.contains("x-agentgear") && sk.contains("ez-fixture-plugin"), "ownership tag missing:\n{sk}");
    assert!(skill_dir.join("reference.md").exists(), "skill support file not copied through");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.cursor.join("mcp.json"), hooks_file.clone(), cmd_file.clone(), agent_file.clone(), skill.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "cursor"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // mutation guard: delete our SKILL.md so the skills probe reads Absent; self-heal must
    // compose that into NeedsRepair and rewrite it. A no-op skills probe would leave it gone.
    fs::remove_file(&skill).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the deleted skill: {out}");
    assert!(skill.exists(), "self-heal did not restore the deleted skill");
    assert!(fs::read_to_string(&skill).unwrap().contains("x-agentgear"), "restored skill lost its ownership tag");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let m = env.mcp();
    assert!(!m.contains("ez-fixture"), "our mcp server survived uninstall:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "uninstall removed the seeded mcp server:\n{m}");
    assert!(m.contains("editorTelemetry"), "uninstall removed the seeded top-level key:\n{m}");
    let h = env.hooks();
    assert!(!h.contains("self-heal") && !h.contains("check-restart"), "our hooks survived uninstall:\n{h}");
    assert!(h.contains("their-session-hook"), "uninstall removed the seeded sessionStart hook:\n{h}");
    assert!(h.contains("their-stop-hook"), "uninstall removed the seeded stop hook:\n{h}");
    assert!(!cmd_file.exists(), "command file survived uninstall: {}", cmd_file.display());
    assert!(!agent_file.exists(), "agent file survived uninstall: {}", agent_file.display());
    assert!(!skill_dir.exists(), "skill dir survived uninstall: {}", skill_dir.display());

    // the post-uninstall mcp.json still parses: a clean re-install lands again
    // (json_edit would error on an unparseable mcp.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "cursor"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp().contains("ez-fixture"), "re-install did not re-add our server");
}

#[test]
fn cursor_uninstall_leaves_a_user_owned_empty_event_array_alone() {
    // Cursor's entries sit directly in the event array, so it carries the one-level
    // form of the same ownership rule: an event array the user already had empty is
    // theirs. Sweeping every empty one deletes a key they wrote. `version` keeps this
    // root non-empty, so unlike the qwen arm the file survives either way — the key is
    // the whole assertion.
    const SEED: &str = r#"{
  "version": 1,
  "hooks": { "customEvent": [] }
}
"#;
    let env = Env::new("user-empty-event");
    fs::write(env.cursor.join("hooks.json"), SEED).unwrap();
    let seed: serde_json::Value = serde_json::from_str(SEED).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "cursor"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall errored: {out}");
    let after: serde_json::Value = serde_json::from_str(&env.hooks()).unwrap();
    assert_eq!(after, seed, "an event array the user had empty is theirs, not ours to sweep");
}

// --- instructions (always-apply rule) -----------------------------------------

/// The fixture's `<name>@<marketplace>` plugin id; the derive defaults `marketplace`
/// to `name` when no attr names it.
const INSTR_ID: &str = "ez-fixture-plugin@ez-fixture-plugin";
const INSTR_GUIDANCE: &str = "ez-fixture always-loaded guidance line.\n";
const INSTR_MDC_NAME: &str = "agentgear-ez-fixture-plugin.mdc";

/// The exact `.mdc` rule file the instructions surface must render: cursor
/// frontmatter with `alwaysApply: true` (its unconditional class) plus the
/// `x-agentgear` ownership tag — the same frontmatter-scoped tag as skills — then
/// the host guidance.
fn instr_mdc() -> String {
    format!("---\nalwaysApply: true\nx-agentgear: \"{INSTR_ID}\"\n---\n{INSTR_GUIDANCE}")
}

/// Our rule file's path under a `.cursor` base (user `~/.cursor` or a project's
/// `<root>/.cursor`).
fn instr_mdc_at(cursor_base: &Path) -> PathBuf {
    cursor_base.join("rules").join(INSTR_MDC_NAME)
}

/// The user-scope instructions lifecycle in `~/.cursor/rules/`: install authors
/// the rule with exactly our render; a drifted or deleted rule is repaired by
/// self-heal; uninstall removes our file and a rules dir our own removal emptied.
#[test]
fn cursor_instructions_mdc_lifecycle() {
    let env = Env::new("instr-lifecycle");
    let rules = env.cursor.join("rules");
    let mdc = instr_mdc_at(&env.cursor);

    let (ok, out) = env.fixture(&["setup", "--agent", "cursor"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(fs::read_to_string(&mdc).unwrap(), instr_mdc(), "setup must author the rule file with exactly our render");

    let (ok, out) = env.fixture(&["setup", "--agent", "cursor"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");
    assert_eq!(fs::read_to_string(&mdc).unwrap(), instr_mdc(), "a no-op setup must not rewrite the rule");

    fs::write(&mdc, format!("---\nalwaysApply: true\nx-agentgear: \"{INSTR_ID}\"\n---\ntampered\n")).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the drifted rule: {out}");
    assert_eq!(fs::read_to_string(&mdc).unwrap(), instr_mdc(), "self-heal must restore the exact bytes");

    fs::remove_file(&mdc).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the deleted rule: {out}");
    assert_eq!(fs::read_to_string(&mdc).unwrap(), instr_mdc(), "self-heal must re-add the rule");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert!(!mdc.exists(), "uninstall must remove our rule file");
    assert!(!rules.exists(), "a rules dir our own removal emptied must not survive as an orphan");
}

/// Project scope writes the same render under the project's own `.cursor/rules/`,
/// and a project-scope uninstall removes it there.
#[test]
fn cursor_instructions_project_scope() {
    let env = Env::new("instr-project");
    let proj = env.root.join("proj");
    let mdc = instr_mdc_at(&proj.join(".cursor"));

    let (ok, out) = env.fixture(&["setup", "--agent", "cursor", "--project", proj.to_str().unwrap()]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(fs::read_to_string(&mdc).unwrap(), instr_mdc(), "project scope must write the same render under <project>/.cursor/rules");

    let (ok, out) = env.fixture(&["uninstall", "--project", proj.to_str().unwrap()]);
    assert!(ok, "uninstall failed: {out}");
    assert!(!mdc.exists(), "project-scope uninstall must remove our rule file");
}

/// A foreign file at our exact slot (no `x-agentgear` tag) is the user's: skipped
/// whole by install, never tagged or rewritten, and left byte-identical by
/// uninstall — the same three-way ownership gate as skills.
#[test]
fn cursor_instructions_foreign_same_named_file_is_skipped() {
    let env = Env::new("instr-foreign-samename");
    let mdc = instr_mdc_at(&env.cursor);
    fs::create_dir_all(mdc.parent().unwrap()).unwrap();
    let theirs = "---\nalwaysApply: true\n---\ntheir own rule body\n";
    fs::write(&mdc, theirs).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "cursor"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert_eq!(fs::read_to_string(&mdc).unwrap(), theirs, "a foreign file at our slot must be skipped whole, never clobbered or tagged");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(fs::read_to_string(&mdc).unwrap(), theirs, "uninstall must leave a file we never owned byte-identical");
}

/// A foreign rule file beside ours must survive our install and uninstall: the
/// rules dir is shared with the user's own rules, never swept.
#[test]
fn cursor_instructions_foreign_rule_survives() {
    let env = Env::new("instr-foreign-sibling");
    let rules = env.cursor.join("rules");
    fs::create_dir_all(&rules).unwrap();
    let theirs = rules.join("my-own-rule.mdc");
    fs::write(&theirs, "---\ndescription: mine\n---\ntheir rule\n").unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "cursor"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(fs::read_to_string(instr_mdc_at(&env.cursor)).unwrap(), instr_mdc(), "our rule lands beside theirs");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert!(theirs.exists(), "uninstall must never sweep a foreign rule file");
    assert!(rules.exists(), "the rules dir survives while it still holds the user's own rule");
}
