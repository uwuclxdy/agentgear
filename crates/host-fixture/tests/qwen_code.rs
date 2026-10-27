//! Hermetic qwen-code-backend lifecycle, fully isolated from the real `~/.qwen`.
//! No docker, no auth, no `qwen` binary: the backend only ever writes qwen-code's
//! config tree, so we drive `host_fixture setup --agent qwen-code` against a temp
//! `QWEN_HOME` (+ HOME/XDG dirs) and assert the written `settings.json` (mcp + hooks)
//! plus the copied command markdown and the rendered agent markdown by parsing them
//! back. `detect()` passes off the pre-created `QWEN_HOME` dir alone (no `qwen` on the
//! sandbox PATH).
//!
//! Every path the backend touches derives from `QWEN_HOME`, pointed at a throwaway
//! temp root — so proving our files land under that root (and the seeded user entries
//! survive) also proves the backend never reaches the developer's real `~/.qwen`.
//!
//! The automatic slot wiring is retired: no backend writes the harness's
//! `ui.statusLine` slot anymore, so a pre-existing one survives every lifecycle op
//! with its value unchanged (compared serialized — the mcp/hooks translation
//! legitimately reserializes the file it sits in).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server, an unrelated top-level key, and a user's own SessionStart
/// hook (sharing the same event our hook writes into) — all MUST outlive our install
/// and uninstall untouched.
const SEED_SETTINGS: &str = r#"{
  "theme": "dark",
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  },
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "their-startup-hook.sh" } ] }
    ]
  }
}
"#;

struct Env {
    root: PathBuf,
    /// The qwen config base, redirected via `QWEN_HOME`.
    qwen: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("qwen")` (and every
    /// other backend's PATH probe) stays false and detection rides on `QWEN_HOME`.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        Self::seeded(name, ".qwen", SEED_SETTINGS)
    }

    /// `qwen_home` is where `QWEN_HOME` points, relative to the temp root, and `seed`
    /// the settings written there. The user-scope tests keep it at `.qwen` so that
    /// dropping `QWEN_HOME` resolves to the same place; the project-scope test moves
    /// it elsewhere precisely so dropping it does NOT.
    fn seeded(name: &str, qwen_home: &str, seed: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-qwen-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            qwen: root.join(qwen_home),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create QWEN_HOME so detect() passes with no `qwen` on PATH, and seed an
        // unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.qwen).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.qwen.join("settings.json"), seed).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("QWEN_HOME", &self.qwen)
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

    fn settings_path(&self) -> PathBuf {
        self.qwen.join("settings.json")
    }

    fn settings(&self) -> String {
        fs::read_to_string(self.settings_path()).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `qwen`, no sibling agent CLIs,
/// so the fan-out stays a pure qwen-code exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

/// The user's own `ui.statusLine`: qwen-code's settings.json DOES gain mcp + hooks
/// (keyed surfaces we own), so the byte-identity contract here is the slot value
/// itself, never the whole file.
const SEED_STATUSLINE: &str = r#"{
  "ui": {
    "statusLine": {
      "type": "command",
      "command": "echo their-bar-row",
      "padding": 2
    }
  }
}
"#;

fn status_line_text(settings: &Path) -> String {
    let parsed: Value = serde_json::from_str(&fs::read_to_string(settings).unwrap()).unwrap();
    serde_json::to_string(parsed.get("ui").and_then(|u| u.get("statusLine")).unwrap()).unwrap()
}

#[test]
fn qwen_code_setup_leaves_an_existing_user_statusline_untouched() {
    // The retired slot: `ui.statusLine` belongs to the user, and every lifecycle op
    // must leave its value byte-identical.
    let env = Env::seeded("untouched", ".qwen", SEED_STATUSLINE);
    let before = status_line_text(&env.settings_path());

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert_eq!(status_line_text(&env.settings_path()), before, "setup rewrote the user's ui.statusLine");

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");
    assert_eq!(status_line_text(&env.settings_path()), before, "self-heal touched the user's ui.statusLine");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(status_line_text(&env.settings_path()), before, "uninstall rewrote the user's ui.statusLine");
}

#[test]
fn qwen_code_full_lifecycle() {
    let env = Env::new("lifecycle");
    let cmd_dir = env.qwen.join("commands").join("ez-fixture-plugin");
    let cmd_file = cmd_dir.join("hello.md");
    let agent_file = env.qwen.join("agents").join("ez-fixture-plugin-ez-helper.md");
    let skill_dir = env.qwen.join("skills").join("ez-skill");
    let skill = skill_dir.join("SKILL.md");

    // install: translates mcp + hooks + commands + agents into qwen's config tree.
    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // A fresh, healthy install must self-heal to a true NoOp: probe reads every surface
    // reconcile just wrote and finds no drift. Guards against a probe/reconcile desync
    // (widened surface probe, or probe rendering from the wrong source) that would churn.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    let s = env.settings();
    // our mcp server landed under `mcpServers`, Plain shape (no `type` field).
    assert!(s.contains("ez-fixture"), "our mcp server key missing:\n{s}");
    assert!(s.contains("host_fixture"), "our mcp command missing:\n{s}");
    // hooks: SessionStart + UserPromptSubmit map 1:1, CC's nested shape, under `hooks`.
    assert!(s.contains("SessionStart"), "SessionStart hook missing:\n{s}");
    assert!(s.contains("UserPromptSubmit"), "UserPromptSubmit hook missing:\n{s}");
    assert!(s.contains("self-heal"), "SessionStart hook command missing:\n{s}");
    assert!(s.contains("check-restart"), "UserPromptSubmit hook command missing:\n{s}");
    // the seeded user config survived our merge.
    assert!(s.contains("theirs") && s.contains("their-server"), "seeded mcp server was clobbered:\n{s}");

    // remote mcp: qwen picks transport purely by key presence (`httpUrl` → http,
    // `url` → sse; `type` is never read), so a `url`-keyed http server would
    // silently load over SSE. The render must use qwen's own keys.
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-http"],
        serde_json::json!({"httpUrl": "http://127.0.0.1:39621/mcp"}),
        "http remote arm mismatch:\n{s}"
    );
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-sse"],
        serde_json::json!({"url": "http://127.0.0.1:39622/sse"}),
        "sse remote arm mismatch:\n{s}"
    );
    assert!(s.contains("\"theme\"") && s.contains("dark"), "seeded top-level key was clobbered:\n{s}");
    assert!(s.contains("their-startup-hook.sh"), "seeded user SessionStart hook was clobbered:\n{s}");

    // commands: markdown copy-through under `commands/<plugin>/`.
    assert!(cmd_file.exists(), "command markdown not written: {}", cmd_file.display());
    let c = fs::read_to_string(&cmd_file).unwrap();
    assert!(c.contains("Say hello"), "command body not copied through:\n{c}");

    // agents: rendered plugin-prefixed markdown with a namespaced name, no CC model alias.
    assert!(agent_file.exists(), "agent markdown not written: {}", agent_file.display());
    let a = fs::read_to_string(&agent_file).unwrap();
    assert!(a.contains(r#"name: "ez-fixture-plugin-ez-helper""#), "agent name not namespaced/quoted:\n{a}");
    assert!(!a.contains("sonnet"), "the CC model alias must be dropped:\n{a}");
    assert!(a.contains("fixture helper agent"), "agent body not translated:\n{a}");

    // skills: bare `<name>/SKILL.md` under ~/.qwen/skills, ownership-tagged, support file copied.
    assert!(skill.exists(), "skill SKILL.md not written: {}", skill.display());
    let sk = fs::read_to_string(&skill).unwrap();
    assert!(sk.contains("name: ez-skill") && sk.contains("description:"), "skill frontmatter missing:\n{sk}");
    assert!(sk.contains("x-agentgear") && sk.contains("ez-fixture-plugin"), "ownership tag missing:\n{sk}");
    assert!(skill_dir.join("reference.md").exists(), "skill support file not copied through");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.qwen.join("settings.json"), cmd_file.clone(), agent_file.clone(), skill.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let s = env.settings();
    assert!(!s.contains("ez-fixture"), "our mcp server survived uninstall:\n{s}");
    assert!(!s.contains("self-heal") && !s.contains("check-restart"), "our hooks survived uninstall:\n{s}");
    assert!(s.contains("theirs") && s.contains("their-server"), "uninstall removed the seeded mcp server:\n{s}");
    assert!(s.contains("\"theme\"") && s.contains("dark"), "uninstall removed the seeded top-level key:\n{s}");
    assert!(s.contains("their-startup-hook.sh"), "uninstall removed the seeded user SessionStart hook:\n{s}");
    assert!(!cmd_dir.exists(), "our command dir survived uninstall: {}", cmd_dir.display());
    assert!(!agent_file.exists(), "our agent file survived uninstall: {}", agent_file.display());
    assert!(!skill_dir.exists(), "our skill dir survived uninstall: {}", skill_dir.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable settings.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.settings().contains("ez-fixture"), "re-install did not re-add our server");
}

#[test]
fn qwen_code_uninstall_drops_a_settings_file_it_authored() {
    // Nothing but our own writes was ever in this file, so the honest inverse of the
    // install that created it is to take it back out. The stash is empty here (there
    // was no slot to displace), which is exactly the arm that used to leave
    // `{"mcpServers":{},"hooks":{},"ui":{}}` sitting on disk forever.
    let env = Env::new("authored-settings");
    fs::remove_file(env.settings_path()).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert!(env.settings_path().exists(), "the arm under test needs install to author the settings file");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert!(!env.settings_path().exists(), "uninstall orphaned a settings file it authored:\n{}", env.settings());
}

#[test]
fn qwen_code_uninstall_leaves_a_user_owned_empty_event_array_alone() {
    // One level below the container prune, same rule: a hook removal sweeps every
    // EMPTY event array, not the ones it emptied, so a user's own empty array under an
    // event we never write to reads as ours to drop. That cascades — `hooks` empties,
    // the container prune takes it, the root empties, and the file goes with a key of
    // theirs still nominally in it.
    const SEED: &str = r#"{
  "hooks": { "CustomEvent": [] }
}
"#;
    let env = Env::seeded("user-empty-event", ".qwen", SEED);
    let seed: Value = serde_json::from_str(SEED).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall errored: {out}");
    assert!(env.settings_path().exists(), "uninstall deleted a settings file holding a key of the user's");
    let after: Value = serde_json::from_str(&env.settings()).unwrap();
    assert_eq!(after, seed, "an event array the user had empty is theirs, not ours to sweep");
}

#[test]
fn qwen_code_uninstall_leaves_a_user_owned_empty_hook_group_alone() {
    // The same rule one level deeper still, and reachable under an event we DO manage:
    // a user group whose handler array is already empty is swept by the blanket group
    // retain, which then empties the event array, the `hooks` container, and the file.
    // Their `matcher` is the proof it was a group of theirs and not a husk of ours.
    const SEED: &str = r#"{
  "hooks": { "SessionStart": [ { "matcher": "mine", "hooks": [] } ] }
}
"#;
    let env = Env::seeded("user-empty-group", ".qwen", SEED);
    let seed: Value = serde_json::from_str(SEED).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall errored: {out}");
    assert!(env.settings_path().exists(), "uninstall deleted a settings file holding a group of the user's");
    let after: Value = serde_json::from_str(&env.settings()).unwrap();
    assert_eq!(after, seed, "a hook group the user had empty is theirs, not ours to sweep");
}

#[test]
fn qwen_code_uninstall_strips_only_our_handler_from_a_shared_group() {
    // The over-removal direction, which every other test here leaves open: they all
    // check that something the user owns SURVIVES a removal that took nothing, while
    // this one checks a removal that genuinely fires takes only its own handler. Our
    // reconcile always appends a fresh group, so the only way into this state is a user
    // consolidating both into one — which the group-level retain must then leave alone,
    // since it still holds a handler of theirs.
    let env = Env::new("shared-group");

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // Move our SessionStart handler into the user's own group, the way a user tidying
    // their config by hand would. Reading it back rather than hardcoding it keeps this
    // honest if the fixture's command ever changes.
    let mut root: Value = serde_json::from_str(&env.settings()).unwrap();
    let groups = root["hooks"]["SessionStart"].as_array_mut().unwrap();
    let ours_idx = groups
        .iter()
        .position(|g| g["hooks"][0]["command"].as_str().is_some_and(|c| c != "their-startup-hook.sh"))
        .expect("install must have appended a SessionStart group of ours");
    let our_handler = groups.remove(ours_idx)["hooks"][0].clone();
    let theirs = groups.iter_mut().find(|g| g["hooks"][0]["command"] == "their-startup-hook.sh").unwrap();
    theirs["hooks"].as_array_mut().unwrap().push(our_handler.clone());
    fs::write(env.settings_path(), serde_json::to_string_pretty(&root).unwrap()).unwrap();

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall errored: {out}");

    let after: Value = serde_json::from_str(&env.settings()).unwrap();
    assert_eq!(
        after["hooks"]["SessionStart"],
        json!([{ "hooks": [ { "type": "command", "command": "their-startup-hook.sh" } ] }]),
        "a group still holding a handler of theirs must survive with exactly that handler left"
    );
    assert!(!env.settings().contains(our_handler["command"].as_str().unwrap()), "our handler survived inside the shared group");
}

// --- instructions ------------------------------------------------------------

/// The fixture's `<name>@<marketplace>` plugin id; the derive defaults `marketplace`
/// to `name` when no attr names it.
const INSTR_ID: &str = "ez-fixture-plugin@ez-fixture-plugin";
const INSTR_GUIDANCE: &str = "ez-fixture always-loaded guidance line.\n";

/// The exact tagged block the instructions surface must render into the global
/// `$QWEN_HOME/QWEN.md`: the host guidance between two html-comment markers naming
/// the plugin id, the whole block replaceable byte-for-byte.
fn instr_block() -> String {
    format!("<!-- agentgear:{INSTR_ID} begin -->\n{INSTR_GUIDANCE}<!-- agentgear:{INSTR_ID} end -->\n")
}

/// The user-scope instructions lifecycle in qwen's global `$QWEN_HOME/QWEN.md` (the
/// first of its two default context filenames at the global tier, reached with zero
/// settings writes): install appends our block after the user's content with one
/// blank separator line; a drifted or deleted block is repaired by self-heal;
/// uninstall strips exactly our block and the separator.
#[test]
fn qwen_instructions_block_lifecycle() {
    let env = Env::new("instr-lifecycle");
    let qwen_md = env.qwen.join("QWEN.md");
    let seed = "user rule one\nuser rule two\n";
    fs::write(&qwen_md, seed).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");
    let installed = format!("{seed}\n{}", instr_block());
    assert_eq!(fs::read_to_string(&qwen_md).unwrap(), installed, "install must append exactly our block after a blank line");

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");
    assert_eq!(fs::read_to_string(&qwen_md).unwrap(), installed, "a no-op setup must not rewrite QWEN.md");

    let tampered = format!("{seed}\n<!-- agentgear:{INSTR_ID} begin -->\ntampered\n<!-- agentgear:{INSTR_ID} end -->\n");
    fs::write(&qwen_md, &tampered).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the drifted block: {out}");
    assert_eq!(fs::read_to_string(&qwen_md).unwrap(), installed, "self-heal must restore the exact block bytes");

    fs::write(&qwen_md, seed).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the deleted block: {out}");
    assert_eq!(fs::read_to_string(&qwen_md).unwrap(), installed, "self-heal must re-add the block");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(fs::read_to_string(&qwen_md).unwrap(), seed, "uninstall must restore the user's file byte-for-byte");
}

/// With no pre-existing QWEN.md, install creates the file holding only our block,
/// and uninstall — our removal being what emptied it — takes the file with it.
#[test]
fn qwen_instructions_owns_only_its_file() {
    let env = Env::new("instr-owned-file");
    let qwen_md = env.qwen.join("QWEN.md");
    assert!(!qwen_md.exists(), "precondition: no QWEN.md seeded");

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(fs::read_to_string(&qwen_md).unwrap(), instr_block(), "a file we created holds exactly our block, no leading blank line");

    // The Ok line pins the `instructions.is_some()` gate, the push and the user-scope path it is handed.
    let (ok, out) = env.fixture(&["doctor"]);
    assert!(ok, "doctor failed over a healthy install: {out}");
    let want = format!("[ ok ] guidance block present: block present in {}", qwen_md.display());
    assert!(out.lines().any(|l| l == want), "the doctor report lacks an ok guidance-block check over a healthy install:\n{out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert!(!qwen_md.exists(), "uninstall must remove the file our removal emptied");
}

/// A rival agentgear host's block (different plugin id in the markers) and the user's
/// own lines must survive our install and uninstall byte-identical: the marker id
/// scopes ownership, so two agentgear plugins coexist in one QWEN.md.
#[test]
fn qwen_never_clobbers_a_rival_instructions_block() {
    let env = Env::new("instr-rival");
    let qwen_md = env.qwen.join("QWEN.md");
    let rival =
        "user line\n<!-- agentgear:rival-plugin@rival-mkt begin -->\nrival guidance\n<!-- agentgear:rival-plugin@rival-mkt end -->\n";
    fs::write(&qwen_md, rival).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(
        fs::read_to_string(&qwen_md).unwrap(),
        format!("{rival}\n{}", instr_block()),
        "install must append our block, leaving the rival's untouched"
    );

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(fs::read_to_string(&qwen_md).unwrap(), rival, "uninstall must strip only our block, keeping the rival's and the user's");
}

/// Project scope has no instructions surface: qwen's global tier reads the config
/// root's `QWEN.md`/`AGENTS.md` and the project context-file tier is a separate
/// surface agentgear does not write in v1 — so a `--project` install must leave no
/// QWEN.md anywhere.
#[test]
fn qwen_instructions_user_scope_only() {
    let env = Env::new("instr-user-scope-only");
    let proj = env.root.join("proj");
    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code", "--project", proj.to_str().unwrap()]);
    assert!(ok, "setup failed: {out}");
    assert!(proj.join(".qwen").join("settings.json").exists(), "precondition: project install ran");
    assert!(!proj.join(".qwen").join("QWEN.md").exists(), "project .qwen/QWEN.md must not be written");
    assert!(!env.qwen.join("QWEN.md").exists(), "a project install must not touch the user's global QWEN.md");
}
