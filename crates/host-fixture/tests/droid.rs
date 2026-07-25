//! Hermetic droid-backend lifecycle, fully isolated from the real `~/.factory`. No
//! docker, no auth, no `droid` binary: the backend only ever writes droid's own config
//! files, so we drive `host_fixture setup --agent droid` against a temp `HOME` (+ XDG
//! dirs) and assert the written `mcp.json` / `hooks.json` / command + droid markdown by
//! parsing them back. `detect()` passes off the pre-created `~/.factory` dir alone (no
//! `droid` on the sandbox PATH).
//!
//! Every path the backend touches derives from `HOME`, pointed at a throwaway temp root
//! — so proving our files land under that root (and the seeded user entries survive)
//! also proves the backend never reaches the developer's real `~/.factory`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our install and
/// uninstall untouched. Lives in droid's dedicated `mcp.json`.
const SEED_MCP: &str = r#"{
  "telemetry": false,
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  }
}
"#;

/// A user's own SessionStart hook (sharing the event our hook writes into) that must
/// survive our merge and our removal. Lives in droid's dedicated `hooks.json`.
const SEED_HOOKS: &str = r#"{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "their-startup-hook.sh" } ] }
    ]
  }
}
"#;

struct Env {
    root: PathBuf,
    factory: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("droid")` (and every other
    /// backend's PATH probe) stays false and detection rides on `~/.factory`.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-droid-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            factory: root.join(".factory"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create ~/.factory so detect() passes with no `droid` on PATH, and seed
        // unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.factory).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.factory.join("mcp.json"), SEED_MCP).unwrap();
        fs::write(env.factory.join("hooks.json"), SEED_HOOKS).unwrap();
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
        fs::read_to_string(self.factory.join("mcp.json")).unwrap()
    }

    fn hooks(&self) -> String {
        fs::read_to_string(self.factory.join("hooks.json")).unwrap()
    }

    /// The same run with `HOME` pointed at a directory that has no `~/.factory`, so
    /// droid reads as uninstalled. XDG stays put, so the stamp marker still resolves —
    /// this models the user removing droid, not the sandbox moving.
    fn fixture_without_droid(&self, args: &[&str]) -> (bool, String) {
        let elsewhere = self.root.join("no-droid-here");
        fs::create_dir_all(&elsewhere).unwrap();
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        cmd.env("HOME", &elsewhere);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Run the fixture with `stdin` piped in — the status-line entrypoint's real calling
    /// convention. Gets the inherited `PATH` appended because composing runs the user's
    /// own command through the platform shell; safe here and nowhere else, since the
    /// status-line entrypoint never detects or writes a backend.
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

    fn settings(&self) -> PathBuf {
        self.factory.join("settings.json")
    }

    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    fn project_settings(&self) -> PathBuf {
        self.project().join(".factory").join("settings.json")
    }

    /// The droid agent's stamp marker, if one exists.
    fn marker(&self) -> Option<Value> {
        let markers = self.data.join("ez-fixture-plugin").join("markers");
        let entries: Vec<_> = fs::read_dir(&markers).map(|d| d.flatten().collect()).unwrap_or_default();
        entries.into_iter().find_map(|entry| {
            let marker: Value = serde_json::from_slice(&fs::read(entry.path()).unwrap_or_default()).ok()?;
            (marker.get("agent").and_then(Value::as_str) == Some("droid")).then_some(marker)
        })
    }

    fn stashed_original(&self) -> Value {
        self.marker().and_then(|m| m.get("statusline_original").cloned()).unwrap_or(Value::Null)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `droid`, no sibling agent CLIs, so
/// the fan-out stays a pure droid exercise regardless of the dev box.
fn with_inherited_path(curated: &OsString) -> OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(curated).chain(std::env::split_paths(&inherited)).collect();
    std::env::join_paths(&dirs).unwrap_or_else(|_| curated.clone())
}

fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn droid_full_lifecycle() {
    let env = Env::new("lifecycle");
    let command = env.factory.join("commands").join("ez-fixture-plugin-hello.md");
    let droid = env.factory.join("droids").join("ez-fixture-plugin-ez-helper.md");
    let skill_dir = env.factory.join("skills").join("ez-skill");
    let skill = skill_dir.join("SKILL.md");

    // install: translates mcp + hooks + commands + agents into droid's config tree.
    let (ok, out) = env.fixture(&["setup", "--agent", "droid"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // A fresh, healthy install must self-heal to a true NoOp: probe reads every surface
    // reconcile just wrote and finds no drift. Guards against a probe/reconcile desync
    // (widened surface probe, or probe rendering from the wrong source) that would churn.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    // mcp: our server landed under `mcpServers` (Plain shape) in the dedicated mcp.json.
    let m = env.mcp();
    assert!(m.contains("ez-fixture"), "our mcp server key missing:\n{m}");
    assert!(m.contains("host_fixture"), "our mcp command missing:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "seeded mcp server was clobbered:\n{m}");
    assert!(m.contains("telemetry"), "seeded top-level key was clobbered:\n{m}");

    // remote mcp: both arms land in droid's exact accepted shape.
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

    // hooks: SessionStart + UserPromptSubmit map 1:1 into the CC-shape hooks.json wrapper.
    let h = env.hooks();
    assert!(h.contains("SessionStart"), "SessionStart hook missing:\n{h}");
    assert!(h.contains("UserPromptSubmit"), "UserPromptSubmit hook missing:\n{h}");
    assert!(h.contains("self-heal"), "SessionStart hook command missing:\n{h}");
    assert!(h.contains("check-restart"), "UserPromptSubmit hook command missing:\n{h}");
    assert!(h.contains("their-startup-hook.sh"), "seeded user SessionStart hook was clobbered:\n{h}");

    // commands -> a flat, namespaced markdown command (verbatim copy of the CC command).
    assert!(command.exists(), "command markdown not written: {}", command.display());
    let c = fs::read_to_string(&command).unwrap();
    assert!(c.contains("description"), "command frontmatter not carried through:\n{c}");
    assert!(c.contains("Say hello"), "command body not carried through:\n{c}");

    // agents -> a flat, namespaced custom droid with a plugin-prefixed `name`.
    assert!(droid.exists(), "custom droid markdown not written: {}", droid.display());
    let d = fs::read_to_string(&droid).unwrap();
    assert!(d.contains("name: ez-fixture-plugin-ez-helper"), "custom droid name not namespaced:\n{d}");
    assert!(d.contains("model: sonnet"), "custom droid model frontmatter not translated:\n{d}");
    assert!(d.contains("fixture helper agent"), "custom droid body not translated:\n{d}");

    // skills: bare `<name>/SKILL.md` under ~/.factory/skills, ownership-tagged, support file copied.
    assert!(skill.exists(), "skill SKILL.md not written: {}", skill.display());
    let sk = fs::read_to_string(&skill).unwrap();
    assert!(sk.contains("name: ez-skill") && sk.contains("description:"), "skill frontmatter missing:\n{sk}");
    assert!(sk.contains("x-agentgear") && sk.contains("ez-fixture-plugin"), "ownership tag missing:\n{sk}");
    assert!(skill_dir.join("reference.md").exists(), "skill support file not copied through");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.factory.join("mcp.json"), env.factory.join("hooks.json"), command.clone(), droid.clone(), skill.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "droid"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let m = env.mcp();
    assert!(!m.contains("ez-fixture"), "our mcp server survived uninstall:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "uninstall removed the seeded mcp server:\n{m}");
    assert!(m.contains("telemetry"), "uninstall removed the seeded top-level key:\n{m}");

    let h = env.hooks();
    assert!(!h.contains("self-heal") && !h.contains("check-restart"), "our hooks survived uninstall:\n{h}");
    assert!(h.contains("their-startup-hook.sh"), "uninstall removed the seeded user SessionStart hook:\n{h}");

    assert!(!command.exists(), "our command file survived uninstall: {}", command.display());
    assert!(!droid.exists(), "our custom droid file survived uninstall: {}", droid.display());
    assert!(!skill_dir.exists(), "our skill dir survived uninstall: {}", skill_dir.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable mcp.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "droid"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp().contains("ez-fixture"), "re-install did not re-add our server");
}

// --- statusLine ---------------------------------------------------------------
//
// Root-level `statusLine` in `<base>/settings.json`, both scopes. droid's schema is
// `{type?: "command", command, padding?, maxRows?: 1..=3}`; `type` is optional AND
// accepted, so this reuses the shared CC-shaped renderer.

/// The user's own settings before we touch anything: an unrelated key plus a status
/// line of theirs. `echo their-bar-row` runs under both `sh -c` and `cmd /C`.
const SEED_SETTINGS: &str = r#"{
  "telemetry": false,
  "statusLine": {
    "command": "echo their-bar-row",
    "padding": 2,
    "maxRows": 2
  }
}
"#;

const SESSION_JSON: &str = r#"{"session_id":"abc"}"#;

const OUR_COMMAND: &str = "host_fixture statusline --client droid";

/// What the backend must write: CC's body plus droid's own row cap. `maxRows: 3`
/// because `compose` emits two rows whenever the user had a line of their own, and
/// droid's default cap of 1 would clip theirs off.
fn our_status_line() -> Value {
    json!({"type": "command", "command": OUR_COMMAND, "padding": 0, "maxRows": 3})
}

fn seed_status_line() -> Value {
    parse(SEED_SETTINGS).get("statusLine").cloned().unwrap()
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn status_line_at(settings: &Path) -> Value {
    parse(&fs::read_to_string(settings).unwrap()).get("statusLine").cloned().unwrap_or(Value::Null)
}

/// Serialized, not `Value`-compared: `preserve_order` maps compare equal regardless of
/// key order, so only the rendered text proves a byte-exact restore.
fn status_line_text(settings: &Path) -> String {
    serde_json::to_string(&status_line_at(settings)).unwrap()
}

fn set_status_line_at(settings: &Path, value: Value) {
    let mut parsed = parse(&fs::read_to_string(settings).unwrap());
    parsed["statusLine"] = value;
    fs::write(settings, serde_json::to_vec_pretty(&parsed).unwrap()).unwrap();
}

fn remove_status_line_at(settings: &Path) {
    let mut parsed = parse(&fs::read_to_string(settings).unwrap());
    parsed.as_object_mut().unwrap().remove("statusLine");
    fs::write(settings, serde_json::to_vec_pretty(&parsed).unwrap()).unwrap();
}

#[test]
fn droid_statusline_full_lifecycle() {
    let env = Env::new("statusline-lifecycle");
    fs::write(env.settings(), SEED_SETTINGS).unwrap();
    let settings = env.settings();
    let seeded_text = status_line_text(&settings);

    let (ok, out) = env.fixture(&["setup", "--agent", "droid"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    assert_eq!(status_line_at(&settings), our_status_line(), "our statusLine did not land");
    assert_eq!(env.stashed_original(), seed_status_line(), "install did not stash the user's original");
    assert!(fs::read_to_string(&settings).unwrap().contains("telemetry"), "the user's unrelated key was clobbered");

    // Two rows, which is exactly why `maxRows` is written at all.
    let (ok, out) = env.fixture_stdin(&["statusline", "--client", "droid"], SESSION_JSON);
    assert!(ok, "statusline subcommand failed: {out}");
    assert_eq!(out, "ez-fixture row\ntheir-bar-row", "compose did not stack our row over the user's");

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    let (ok, out) = env.fixture(&["setup", "--agent", "droid"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(status_line_text(&settings), seeded_text, "uninstall did not restore the user's statusLine byte-for-byte");

    // Never resurrect: a foreign line is not evidence our plugin is installed.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored after uninstall: {out}");
    assert_eq!(out, "NoOp", "a foreign status line resurrected an uninstalled plugin, got {out}");
    assert_eq!(status_line_text(&settings), seeded_text, "self-heal retook the slot after uninstall");
    assert!(env.marker().is_none(), "self-heal adopted a plugin that is not installed");
}

#[test]
fn droid_statusline_stash_survives_a_declaration_change() {
    // Ownership by whole-value equality would read our OWN previous rendering as the
    // user's original the moment a release changes what we render — destroying their
    // value and poisoning the stash with our own command, which `compose` then runs from
    // inside itself. Only observable across a version change.
    let env = Env::new("statusline-version-change");
    fs::write(env.settings(), SEED_SETTINGS).unwrap();
    let settings = env.settings();
    let seeded_text = status_line_text(&settings);

    let (ok, out) = env.fixture(&["setup", "--agent", "droid"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // What host version N left: our command, a body this version no longer renders.
    set_status_line_at(&settings, json!({"type": "command", "command": OUR_COMMAND, "padding": 1, "maxRows": 1}));

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored on our own drifted rendering: {out}");
    assert_eq!(out, "Installed", "our own drifted rendering is drift to repair, got {out}");
    assert_eq!(status_line_at(&settings), our_status_line(), "self-heal did not converge our own rendering");

    let (ok, out) = env.fixture_stdin(&["statusline", "--client", "droid"], SESSION_JSON);
    assert!(ok, "statusline subcommand failed: {out}");
    assert_eq!(out, "ez-fixture row\ntheir-bar-row", "compose lost the user's row");
    assert_eq!(env.stashed_original(), seed_status_line(), "our own earlier rendering was stashed as the user's original");

    let (ok, out) = env.fixture(&["update"]);
    assert!(ok, "update failed: {out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(status_line_text(&settings), seeded_text, "the stash did not survive the version change plus update");
}

#[test]
fn droid_statusline_deleted_line_is_readded_without_losing_the_stash() {
    let env = Env::new("statusline-drift");
    fs::write(env.settings(), SEED_SETTINGS).unwrap();
    let settings = env.settings();
    let seeded_text = status_line_text(&settings);

    let (ok, out) = env.fixture(&["setup", "--agent", "droid"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    remove_status_line_at(&settings);
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored on a deleted statusLine: {out}");
    assert_eq!(out, "Installed", "a missing statusLine behind healthy surfaces is drift, got {out}");
    assert_eq!(status_line_at(&settings), our_status_line(), "self-heal did not re-add our statusLine");
    assert_eq!(env.stashed_original(), seed_status_line(), "the repair pass overwrote the user's stashed original");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(status_line_text(&settings), seeded_text, "uninstall did not restore the user's statusLine");
}

#[test]
fn droid_statusline_teardown_restores_when_the_harness_is_gone() {
    // `uninstall`'s skip rows never call `remove`, so a user who removed droid before
    // running our uninstall would keep our command in their settings while the marker
    // holding their original is cleared out from under it. Driven at project scope,
    // whose config base stays resolvable with no `~/.factory` left to detect.
    let env = Env::new("statusline-harness-gone");
    let settings = env.project_settings();
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, SEED_SETTINGS).unwrap();
    let seeded_text = status_line_text(&settings);
    let project = env.project().display().to_string();

    let (ok, out) = env.fixture(&["setup", "--agent", "droid", "--project", &project]);
    assert!(ok && out == "Installed", "project-scope setup failed: {out}");
    assert_eq!(status_line_at(&settings), our_status_line(), "our statusLine did not land at project scope");
    assert_eq!(env.stashed_original(), seed_status_line(), "install did not stash the user's original");

    let (ok, out) = env.fixture_without_droid(&["uninstall", "--project", &project]);
    assert!(ok, "uninstall errored with the harness gone: {out}");
    assert_eq!(out, "NoOp", "every agent should skip with nothing detected, got {out}");
    assert_eq!(status_line_text(&settings), seeded_text, "a skipped uninstall stranded our command and dropped the stash");
    assert!(env.marker().is_none(), "the marker should be cleared once the value is back");
}

#[test]
fn droid_teardown_with_nothing_installed_writes_nothing() {
    // A teardown that owns nothing in a user's file must not write to it at all.
    let env = Env::new("statusline-empty-teardown");
    fs::write(env.settings(), SEED_SETTINGS).unwrap();
    let before = fs::read_to_string(env.settings()).unwrap();

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall errored with nothing installed: {out}");
    assert_eq!(out, "NoOp", "a teardown with nothing of ours to undo must report no change, got {out}");
    assert_eq!(fs::read_to_string(env.settings()).unwrap(), before, "teardown wrote into a file it had nothing to undo in");
}
