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
//! The `ui.statusLine` tests below pin a different contract from the rest: that slot
//! holds a single value and is last-writer-wins, so install displaces whatever the
//! user had into the stamp marker, every later `update`/`self-heal` carries that
//! stash forward untouched, and teardown puts their value back — but only while the
//! live value is still ours to take back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

    /// The same run with `QWEN_HOME` dropped: `~/.qwen` under the temp HOME is then
    /// the config base, so a layout whose base is elsewhere reads as an uninstalled
    /// qwen-code (`detect()` keys on that dir existing, or `qwen` on PATH).
    fn fixture_without_qwen_home(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        cmd.env_remove("QWEN_HOME");
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Run the fixture with `stdin` piped in — the status-line entrypoint's real
    /// calling convention.
    ///
    /// This one call gets the inherited `PATH` appended, because composing runs the
    /// user's own status command through the platform shell and the curated `PATH`
    /// above carries no `sh`/`cmd`. Safe to widen here and nowhere else: the
    /// status-line entrypoint never detects or writes a backend, so a stray `qwen` on
    /// the dev box cannot reach it.
    fn fixture_stdin(&self, args: &[&str], stdin: &str) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        self.apply(&mut cmd);
        cmd.env("PATH", with_inherited_path(&self.path));
        let mut child = cmd.spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim_end_matches(['\n', '\r']).to_string())
    }

    fn settings_path(&self) -> PathBuf {
        self.qwen.join("settings.json")
    }

    fn settings(&self) -> String {
        fs::read_to_string(self.settings_path()).unwrap()
    }

    /// The project root a `--project`-scoped run installs into: its config base is
    /// `<project>/.qwen`, independent of `QWEN_HOME` and of detection.
    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    fn project_settings(&self) -> PathBuf {
        self.project().join(".qwen").join("settings.json")
    }

    /// The qwen-code agent's stamp marker, if one exists. Its presence is what says
    /// this backend considers the plugin installed — an adopt writes one.
    fn marker(&self) -> Option<Value> {
        let markers = self.data.join("ez-fixture-plugin").join("markers");
        let entries: Vec<_> = fs::read_dir(&markers).map(|d| d.flatten().collect()).unwrap_or_default();
        entries.into_iter().find_map(|entry| {
            let marker: Value = serde_json::from_slice(&fs::read(entry.path()).unwrap_or_default()).ok()?;
            (marker.get("agent").and_then(Value::as_str) == Some("qwen-code")).then_some(marker)
        })
    }

    /// The stash the qwen-code backend recorded, as raw JSON.
    fn stashed_original(&self) -> Value {
        self.marker().and_then(|m| m.get("statusline_original").cloned()).unwrap_or(Value::Null)
    }

    /// The files a full translate lands, so a resurrection is visible as a set rather
    /// than one lucky path. Their parent dirs survive an uninstall by design (only the
    /// plugin-owned leaves are removed), so the leaves are what gets asserted.
    fn translated_paths(&self) -> [PathBuf; 3] {
        [
            self.qwen.join("commands").join("ez-fixture-plugin"),
            self.qwen.join("agents").join("ez-fixture-plugin-ez-helper.md"),
            self.qwen.join("skills").join("ez-skill"),
        ]
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

/// `curated` first (so no dev-box binary can shadow the fixture), then whatever the
/// test process inherited.
fn with_inherited_path(curated: &OsString) -> OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(curated).chain(std::env::split_paths(&inherited)).collect();
    std::env::join_paths(&dirs).unwrap_or_else(|_| curated.clone())
}

// --- ui.statusLine -----------------------------------------------------------

/// The user's own settings before we touch anything: an unrelated top-level key, an
/// unrelated sibling INSIDE the `ui` container we write into, and a real status line
/// of theirs.
///
/// `echo their-bar-row` is deliberately runnable under both `sh -c` and `cmd /C`, so
/// the compose assertion below exercises the real subprocess path on every platform.
const SEED_WITH_STATUSLINE: &str = r#"{
  "theme": "dark",
  "ui": {
    "hideWindowTitle": true,
    "statusLine": {
      "type": "command",
      "command": "echo their-bar-row",
      "padding": 2
    }
  }
}
"#;

/// A session payload shaped like the harness's. No `cwd`, so `compose` resolves the
/// user-scope stash.
const SESSION_JSON: &str = r#"{"session_id":"abc"}"#;

/// What the fixture host declares, with `${AGENTGEAR_CLIENT}` expanded for qwen-code.
const OUR_COMMAND: &str = "host_fixture statusline --client qwen-code";

/// The value the backend must have written: CC's single-object command shape, under
/// qwen-code's own `ui` container.
fn our_status_line() -> Value {
    json!({"type": "command", "command": OUR_COMMAND, "padding": 0})
}

fn seed_status_line() -> Value {
    parse(SEED_WITH_STATUSLINE).get("ui").and_then(|ui| ui.get("statusLine")).cloned().unwrap()
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn status_line_at(settings: &Path) -> Value {
    let parsed = parse(&fs::read_to_string(settings).unwrap());
    parsed.get("ui").and_then(|ui| ui.get("statusLine")).cloned().unwrap_or(Value::Null)
}

/// Serialized rather than compared as a `Value`: `serde_json`'s `preserve_order` maps
/// compare equal regardless of key order, so only the rendered text proves a restore
/// put the user's object back exactly as they wrote it.
fn status_line_text(settings: &Path) -> String {
    serde_json::to_string(&status_line_at(settings)).unwrap()
}

/// Overwrite the slot with someone else's value (or an earlier release's rendering of
/// ours), as a second tool or the user would.
fn set_status_line_at(settings: &Path, value: Value) {
    let mut parsed = parse(&fs::read_to_string(settings).unwrap());
    parsed["ui"]["statusLine"] = value;
    fs::write(settings, serde_json::to_vec_pretty(&parsed).unwrap()).unwrap();
}

fn remove_status_line_at(settings: &Path) {
    let mut parsed = parse(&fs::read_to_string(settings).unwrap());
    parsed["ui"].as_object_mut().unwrap().remove("statusLine");
    fs::write(settings, serde_json::to_vec_pretty(&parsed).unwrap()).unwrap();
}

#[test]
fn qwen_code_statusline_full_lifecycle() {
    let env = Env::seeded("statusline-lifecycle", ".qwen", SEED_WITH_STATUSLINE);
    let settings = env.settings_path();
    let seeded_text = status_line_text(&settings);

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // Our declaration owns the slot, `${AGENTGEAR_CLIENT}` expanded to this backend —
    // not to claude's, which is what makes the marker read below find the right stash.
    assert_eq!(status_line_at(&settings), our_status_line(), "our ui.statusLine did not land:\n{}", env.settings());
    // What we displaced went into the marker, and the container we wrote into kept
    // the user's own sibling keys.
    assert_eq!(env.stashed_original(), seed_status_line(), "install did not stash the user's original");
    let s = env.settings();
    assert!(s.contains("hideWindowTitle"), "a sibling key inside `ui` was clobbered:\n{s}");
    assert!(s.contains("\"theme\""), "the seeded top-level key was clobbered:\n{s}");

    // The compose helper runs the displaced command and appends its rows under ours.
    let (ok, out) = env.fixture_stdin(&["statusline", "--client", "qwen-code"], SESSION_JSON);
    assert!(ok, "statusline subcommand failed: {out}");
    assert_eq!(out, "ez-fixture row\ntheir-bar-row", "compose did not stack our row over the user's");

    // A fresh, healthy install self-heals to a true NoOp: the slot's probe reads back
    // exactly what its reconcile wrote. A convergence test keyed on the command alone
    // would pass here even with a drifted body, so this is whole-value or nothing.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(status_line_text(&settings), seeded_text, "uninstall did not restore the user's ui.statusLine byte-for-byte");

    // Never resurrect. The user's own line is back in the slot, and a foreign value
    // there is NOT evidence our plugin is installed: the slot is the one key we do not
    // own. Counting it as presence defeats the all-Absent arm of `report::compose`, so
    // self_heal's `(no marker, NeedsRepair)` adopt row reinstalls the ENTIRE
    // translation — mcp, hooks, commands, agents, skills — and retakes the slot, over
    // a plugin the user deliberately removed. Reachable without an uninstall too: a
    // host that installed only `claude` while `qwen-code` sits in `plugin.agents` gets
    // qwen-code installed unbidden on the next SessionStart, for any user who has a
    // status line of their own.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored after uninstall: {out}");
    assert_eq!(out, "NoOp", "a foreign status line resurrected an uninstalled plugin, got {out}");
    assert_eq!(status_line_text(&settings), seeded_text, "self-heal retook the slot after uninstall");
    assert!(env.marker().is_none(), "self-heal adopted a plugin that is not installed");
    for path in env.translated_paths() {
        assert!(!path.exists(), "self-heal resurrected the plugin tree: {}", path.display());
    }
    assert!(!env.settings().contains("ez-fixture"), "self-heal re-added our mcp server:\n{}", env.settings());
}

#[test]
fn qwen_code_teardown_with_nothing_installed_writes_nothing() {
    // The non-creating container guard. `ui.statusLine` is nested, so `remove` and
    // `forget` have to walk INTO `ui` — and the creating walker would leave an empty
    // `"ui": {}` behind in a file the USER owns, on a teardown that had nothing to
    // undo, flipping that agent's row from `NoOp` to `Removed` with it. Writing into a
    // user-owned file with nothing to undo is the one thing this surface exists never
    // to do.
    //
    // Reachable: a plugin installed at project scope only, or any user-scope uninstall
    // on a machine where it was never installed. claude cannot cover it — its
    // container path is `[]`, which always resolves — so the pin has to live here.
    // The seed deliberately has no `ui` key.
    let env = Env::new("no-ui-teardown");
    let before = env.settings();

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall errored with nothing installed: {out}");
    assert_eq!(out, "NoOp", "a teardown with nothing of ours to undo must report no change, got {out}");
    assert_eq!(env.settings(), before, "teardown wrote into a settings file it had nothing to undo in:\n{}", env.settings());
}

#[test]
fn qwen_code_statusline_stash_survives_a_declaration_change() {
    // The failure this pins: ownership decided by whole-value equality reads our OWN
    // previous rendering as "the user's original" the moment a release changes what we
    // render (here the padding, the field CC and qwen-code both carry). That destroys
    // the user's value AND poisons the stash with our own command, which `compose`
    // would then run from inside itself on every turn the harness re-renders. It
    // cannot fire at a single version — only across the change — which is why an
    // install-then-uninstall test at one version proves almost nothing here.
    //
    // Ceiling, deliberately not asserted: a release that changes the COMMAND STRING
    // still reads as foreign (docs/design.md § host-owned statusLine). The declared
    // command carries `${AGENTGEAR_CLIENT}` and is otherwise stable across releases;
    // the rendered body is what moves.
    let env = Env::seeded("statusline-version-change", ".qwen", SEED_WITH_STATUSLINE);
    let settings = env.settings_path();
    let seeded_text = status_line_text(&settings);

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert_eq!(env.stashed_original(), seed_status_line(), "install did not stash the user's original");

    // Exactly what host version N left in the slot: our command, a padding this
    // version no longer renders.
    set_status_line_at(&settings, json!({"type": "command", "command": OUR_COMMAND, "padding": 1}));

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored on our own drifted rendering: {out}");
    assert_eq!(out, "Installed", "our own drifted rendering is drift to repair, got {out}");
    assert_eq!(status_line_at(&settings), our_status_line(), "self-heal did not converge our own rendering");

    // Asserted BEFORE the stash below on purpose: this is the anti-recursion guard's
    // positive control. Break the ownership test and the stash holds OUR command, so
    // this call is what would re-enter the binary instead of returning a row.
    let (ok, out) = env.fixture_stdin(&["statusline", "--client", "qwen-code"], SESSION_JSON);
    assert!(ok, "statusline subcommand failed: {out}");
    assert_eq!(out, "ez-fixture row\ntheir-bar-row", "compose lost the user's row");

    assert_eq!(env.stashed_original(), seed_status_line(), "our own earlier rendering was stashed as the user's original");

    // And the stash survives an `update` between install and uninstall: every marker
    // rebuild has to carry it forward, or the restore below has nothing to restore.
    let (ok, out) = env.fixture(&["update"]);
    assert!(ok, "update failed: {out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(status_line_text(&settings), seeded_text, "the stash did not survive the version change plus update");
}

#[test]
fn qwen_code_statusline_deleted_line_is_readded_without_losing_the_stash() {
    // A user who deletes our line leaves an empty slot: probe must read that as drift,
    // reconcile must re-add ours, and the earlier stash must NOT be overwritten by the
    // now-empty slot (an empty slot stashes nothing — that is what makes this
    // harmless).
    let env = Env::seeded("statusline-drift", ".qwen", SEED_WITH_STATUSLINE);
    let settings = env.settings_path();
    let seeded_text = status_line_text(&settings);

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    remove_status_line_at(&settings);
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored on a deleted ui.statusLine: {out}");
    assert_eq!(out, "Installed", "a missing ui.statusLine behind healthy surfaces is drift, got {out}");
    assert_eq!(status_line_at(&settings), our_status_line(), "self-heal did not re-add our ui.statusLine");
    assert_eq!(env.stashed_original(), seed_status_line(), "the repair pass overwrote the user's stashed original");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(status_line_text(&settings), seeded_text, "uninstall did not restore the user's ui.statusLine");
}

#[test]
fn qwen_code_statusline_teardown_restores_when_the_harness_is_gone() {
    // `uninstall`'s skip rows never call `remove`, so a user who uninstalled qwen-code
    // before running our uninstall would keep our command in their settings while the
    // marker holding their original is cleared out from under it. Their settings file
    // outlives the harness — here at project scope, whose config base is `<project>/
    // .qwen` and so stays resolvable with no user-scope qwen-code left to detect.
    let env = Env::seeded("statusline-harness-gone", "qwenhome", "{}\n");
    let settings = env.project_settings();
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, SEED_WITH_STATUSLINE).unwrap();
    let seeded_text = status_line_text(&settings);
    let project = env.project().display().to_string();

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code", "--project", &project]);
    assert!(ok, "project-scope setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");
    assert_eq!(status_line_at(&settings), our_status_line(), "our ui.statusLine did not land at project scope");
    assert_eq!(env.stashed_original(), seed_status_line(), "install did not stash the user's original");

    // QWEN_HOME gone and no `~/.qwen` under the temp HOME: qwen-code reads as
    // uninstalled, so every row of this uninstall is a NotDetected skip.
    let (ok, out) = env.fixture_without_qwen_home(&["uninstall", "--project", &project]);
    assert!(ok, "uninstall errored with the harness gone: {out}");
    assert_eq!(out, "NoOp", "every agent should skip with nothing detected, got {out}");
    assert_eq!(status_line_text(&settings), seeded_text, "a skipped uninstall stranded our command and dropped the stash");
    assert_eq!(env.stashed_original(), Value::Null, "the marker should be cleared once the value is back");
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
fn qwen_code_uninstall_leaves_no_shell_of_the_containers_it_created() {
    // The proven symptom, end to end: the status-line write creates `ui` from nothing,
    // and a removal that only deletes its own leaf keys leaves `"ui": {}` behind in a
    // file the user owns. `mcpServers`/`hooks` are the controls — both hold entries of
    // theirs, so both must survive holding exactly those; `security` is the second
    // control, an empty container nothing of ours ever writes into, which must survive
    // empty rather than be swept by a teardown looking for husks.
    const SEED: &str = r#"{
  "theme": "dark",
  "security": {},
  "mcpServers": { "theirs": { "command": "their-server", "args": [] } },
  "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": "their-startup-hook.sh" } ] } ] }
}
"#;
    let env = Env::seeded("no-container-shells", ".qwen", SEED);
    let seed: Value = serde_json::from_str(SEED).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "qwen-code"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    let installed: Value = serde_json::from_str(&env.settings()).unwrap();
    assert!(installed.get("ui").is_some(), "the arm under test needs install to create `ui`:\n{installed:#}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    let after: Value = serde_json::from_str(&env.settings()).unwrap();
    assert_eq!(after, seed, "uninstall must leave the settings file exactly as it found it");
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
