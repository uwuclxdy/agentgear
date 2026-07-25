//! Hermetic antigravity-cli lifecycle, fully isolated from the real `~/.gemini`.
//! No docker, no auth, no `agy` binary: the backend only ever writes Antigravity's
//! config files, so we drive `host_fixture setup --agent antigravity-cli` against a
//! temp `HOME` (+ XDG dirs) and assert the written `config/mcp_config.json` /
//! `config/hooks.json` by parsing them back. `detect()` passes off the
//! pre-created `~/.gemini/antigravity-cli` dir alone (no `agy` on PATH).
//!
//! Every path the backend touches derives from `HOME`, which we point at a throwaway
//! temp root — so proving our files land under that root (and the seeded user
//! entries survive) also proves the backend never reaches the developer's real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our install
/// and uninstall untouched. Lives in the SHARED `mcp_config.json` the desktop
/// antigravity backend also writes.
const SEED_MCP: &str = r#"{
  "theme": "dark",
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  }
}
"#;

struct Env {
    root: PathBuf,
    mcp: PathBuf,
    hooks: PathBuf,
    /// `~/.gemini/antigravity-cli/settings.json` — the CLI's OWN settings profile,
    /// which is where the host-owned status-line slot lives. Deliberately NOT the
    /// `~/.gemini/config/` customization root the two files above use.
    settings: PathBuf,
    /// `~/.gemini/antigravity-cli/hooks.json` — the retired user-scope hooks path
    /// this backend wrote to until 2026-07-17 (gotcha 1). `agy` never scanned it;
    /// `reconcile` now sweeps a stray file left there by an old binary.
    retired_hooks: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("agy")` (and every
    /// other backend's PATH probe) stays false and detection rides on the config dir.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-antigravity-cli-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let gemini = root.join(".gemini");
        let env = Env {
            mcp: gemini.join("config").join("mcp_config.json"),
            // `~/.gemini/config/` is agy's global customization root and holds both
            // files; `~/.gemini/antigravity-cli/` (the detect marker below) is the
            // CLI's own settings dir, scanned for neither.
            hooks: gemini.join("config").join("hooks.json"),
            settings: gemini.join("antigravity-cli").join("settings.json"),
            retired_hooks: gemini.join("antigravity-cli").join("hooks.json"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create ~/.gemini/antigravity-cli so detect() passes with no `agy` on
        // PATH, and seed an unrelated user mcp config the lifecycle must preserve.
        fs::create_dir_all(gemini.join("antigravity-cli")).unwrap();
        fs::create_dir_all(env.mcp.parent().unwrap()).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(&env.mcp, SEED_MCP).unwrap();
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
        fs::read_to_string(&self.mcp).unwrap()
    }

    fn hooks(&self) -> String {
        fs::read_to_string(&self.hooks).unwrap()
    }

    /// Run the fixture with `stdin` piped in — the status-line entrypoint's real
    /// calling convention. This one call gets the inherited `PATH` appended, because
    /// composing runs the user's own status command through the platform shell and the
    /// curated `PATH` carries no `sh`/`cmd`. Safe to widen here and nowhere else: the
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

    /// The antigravity-cli agent's stamp marker, if one exists. Its presence is what
    /// says this backend considers the plugin installed.
    fn marker(&self) -> Option<Value> {
        let markers = self.data.join("ez-fixture-plugin").join("markers");
        let entries: Vec<_> = fs::read_dir(&markers).map(|d| d.flatten().collect()).unwrap_or_default();
        entries.into_iter().find_map(|entry| {
            let marker: Value = serde_json::from_slice(&fs::read(entry.path()).unwrap_or_default()).ok()?;
            (marker.get("agent").and_then(Value::as_str) == Some("antigravity-cli")).then_some(marker)
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

/// PATH with only the fixture binary's directory: no `agy`, no sibling agent CLIs,
/// so the fan-out stays a pure antigravity-cli exercise regardless of the dev box.
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

#[test]
fn antigravity_cli_full_lifecycle() {
    let env = Env::new("lifecycle");

    // install: translates mcp (shared file, Plain shape) + hooks (plugin-keyed tree).
    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // A fresh, healthy install must self-heal to a true NoOp: probe reads every surface
    // reconcile just wrote and finds no drift. Guards against a probe/reconcile desync
    // (widened surface probe, or probe rendering from the wrong source) that would churn.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    let m = env.mcp();
    // our mcp server landed under `mcpServers`, Plain shape.
    assert!(m.contains("ez-fixture"), "our mcp server key missing:\n{m}");
    assert!(m.contains("host_fixture"), "our mcp command missing:\n{m}");
    // the seeded user config survived our merge.
    assert!(m.contains("theirs") && m.contains("their-server"), "seeded mcp server was clobbered:\n{m}");

    // remote mcp: `agy` accepts only stdio (`command`) or SSE (`serverUrl`); the
    // shared `{type,url,headers}` shape is refused and voids the whole file.
    let parsed: serde_json::Value = serde_json::from_str(&m).unwrap();
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-sse"],
        serde_json::json!({"serverUrl": "http://127.0.0.1:39622/sse"}),
        "sse remote arm mismatch:\n{m}"
    );
    assert!(parsed["mcpServers"].get("ez-fixture-http").is_none(), "http remote must be skipped (no agy landing):\n{m}");
    assert!(m.contains("\"theme\"") && m.contains("dark"), "seeded top-level key was clobbered:\n{m}");

    // hooks: our plugin-keyed tree. The fixture ships SessionStart + UserPromptSubmit;
    // `agy` has no session-level hook, so only UserPromptSubmit lands, flat under its
    // `PreInvocation` analog. Every name written here must be one agy really fires.
    let h = env.hooks();
    let parsed: serde_json::Value = serde_json::from_str(&h).unwrap();
    assert_eq!(
        parsed["ez-fixture-plugin"],
        serde_json::json!({ "PreInvocation": [{ "type": "command", "command": "host_fixture check-restart" }] }),
        "hook tree mismatch:\n{h}"
    );
    assert!(!h.contains("SessionStart"), "SessionStart is not an agy event; it must be skipped, not written:\n{h}");
    assert!(!h.contains("BeforeAgent"), "BeforeAgent does not exist in agy; it must never be written:\n{h}");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [&env.mcp, &env.hooks] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let m = env.mcp();
    assert!(!m.contains("ez-fixture"), "our mcp server survived uninstall:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "uninstall removed the seeded mcp server:\n{m}");
    assert!(m.contains("\"theme\"") && m.contains("dark"), "uninstall removed the seeded top-level key:\n{m}");

    let h = env.hooks();
    assert!(!h.contains("ez-fixture-plugin"), "our hook key survived uninstall:\n{h}");
    assert!(!h.contains("check-restart"), "our hook command survived uninstall:\n{h}");

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable mcp_config.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp().contains("ez-fixture"), "re-install did not re-add our server");
}

/// Retired-path sweep (gotcha 1): `reconcile` clears a stray hooks.json an old
/// binary left at `~/.gemini/antigravity-cli/hooks.json` — but only the exact
/// `<plugin>` key it owns there, matching `remove_hooks`'s existing semantics.
#[test]
fn reconcile_sweeps_our_own_key_at_the_retired_hooks_path() {
    let env = Env::new("retired-sweep-tagged");
    fs::write(&env.retired_hooks, r#"{"ez-fixture-plugin":{"Stop":[{"type":"command","command":"stale"}]}}"#).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok, "setup failed: {out}");

    let retired: serde_json::Value = serde_json::from_str(&fs::read_to_string(&env.retired_hooks).unwrap()).unwrap();
    assert!(retired.get("ez-fixture-plugin").is_none(), "our key at the retired path survived reconcile:\n{retired}");
}

/// The retired-path sweep never touches a same-named file it does not own: a
/// foreign plugin's key at that exact dead path must survive byte-for-byte.
#[test]
fn reconcile_leaves_a_foreign_key_at_the_retired_hooks_path_untouched() {
    let env = Env::new("retired-sweep-foreign");
    const FOREIGN: &str = r#"{"someone-elses-plugin":{"Stop":[{"type":"command","command":"their-hook"}]}}"#;
    fs::write(&env.retired_hooks, FOREIGN).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok, "setup failed: {out}");

    assert_eq!(fs::read_to_string(&env.retired_hooks).unwrap(), FOREIGN, "a foreign key at the retired path was touched by the sweep");
}

/// §6: `enabled:false` carry-through. self_heal must never re-enable a hook
/// subtree the user disabled by hand, and the composite probe must read it as
/// `Disabled` (a true no-op, never reaching `reconcile`) even while the marker is
/// present; an explicit `setup` still honors the user's request to re-enable.
#[test]
fn self_heal_preserves_a_disabled_hook_subtree_but_explicit_setup_reenables() {
    let env = Env::new("enabled-false-carry-through");

    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // The user disables our plugin's hooks by hand.
    let mut doc: serde_json::Value = serde_json::from_str(&env.hooks()).unwrap();
    doc["ez-fixture-plugin"]["enabled"] = serde_json::json!(false);
    fs::write(&env.hooks, serde_json::to_vec(&doc).unwrap()).unwrap();
    let before = env.hooks();

    // self_heal (marker present, probe -> Disabled) must be a true no-op: it must
    // never even reach `reconcile` for this backend, let alone flip the flag.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");
    assert_eq!(out, "NoOp", "self_heal must not touch a deliberately disabled hook subtree, got {out}");
    assert_eq!(env.hooks(), before, "self_heal rewrote the disabled hook subtree");

    // An explicit setup (install/update) still honors the user's request to
    // re-enable, exactly like every other backend's `enabled:false` invariant.
    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok, "re-setup failed: {out}");
    assert_ne!(out, "NoOp", "explicit setup must re-enable a disabled hook subtree, got {out}");
    let after: serde_json::Value = serde_json::from_str(&env.hooks()).unwrap();
    assert_ne!(after["ez-fixture-plugin"]["enabled"], serde_json::json!(false), "explicit setup did not re-enable:\n{after}");
}

// --- statusLine ---------------------------------------------------------------
//
// The slot lives in the CLI's OWN `~/.gemini/antigravity-cli/settings.json`, is
// root-level, and is USER SCOPE ONLY. `agy` 1.1.6 persists `type`/`command`/
// `enabled`/`padding` there and drops every other key on its next rewrite.

/// The user's own settings before we touch anything: an unrelated key, plus a status
/// line of theirs carrying the `enabled` toggle we deliberately never write. Both must
/// come back byte-for-byte on uninstall.
///
/// `echo their-bar-row` is runnable under both `sh -c` and `cmd /C`, so the compose
/// assertion exercises the real subprocess path on every platform.
const SEED_SETTINGS: &str = r#"{
  "editorMode": "vim",
  "statusLine": {
    "type": "command",
    "command": "echo their-bar-row",
    "enabled": true,
    "padding": 2
  }
}
"#;

const SESSION_JSON: &str = r#"{"session_id":"abc"}"#;

/// What the fixture host declares, with `${AGENTGEAR_CLIENT}` expanded for this backend.
const OUR_COMMAND: &str = "host_fixture statusline --client antigravity-cli";

/// `enabled` is absent on purpose: we write `type`/`command`/`padding` and never touch
/// the user's on/off toggle in either direction.
fn our_status_line() -> Value {
    json!({"type": "command", "command": OUR_COMMAND, "padding": 0})
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

/// Serialized, not compared as a `Value`: `serde_json`'s `preserve_order` maps compare
/// equal regardless of key order, so only the rendered text proves a restore put the
/// user's object back exactly as they wrote it.
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

/// Seed the CLI's own settings profile with the user's status line.
fn seed_settings(env: &Env) {
    fs::write(&env.settings, SEED_SETTINGS).unwrap();
}

#[test]
fn antigravity_cli_statusline_full_lifecycle() {
    let env = Env::new("statusline-lifecycle");
    seed_settings(&env);
    let seeded_text = status_line_text(&env.settings);

    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // Ours owns the slot, `${AGENTGEAR_CLIENT}` expanded to THIS backend, and the
    // user's `enabled` toggle is not carried into our own value.
    assert_eq!(status_line_at(&env.settings), our_status_line(), "our statusLine did not land");
    assert_eq!(env.stashed_original(), seed_status_line(), "install did not stash the user's original");
    let s = fs::read_to_string(&env.settings).unwrap();
    assert!(s.contains("editorMode"), "the user's unrelated key was clobbered:\n{s}");

    // Compose runs the displaced command and stacks its rows under ours.
    let (ok, out) = env.fixture_stdin(&["statusline", "--client", "antigravity-cli"], SESSION_JSON);
    assert!(ok, "statusline subcommand failed: {out}");
    assert_eq!(out, "ez-fixture row\ntheir-bar-row", "compose did not stack our row over the user's");

    // Fresh install self-heals to a true NoOp: probe reads back what reconcile wrote.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(status_line_text(&env.settings), seeded_text, "uninstall did not restore the user's statusLine byte-for-byte");

    // Never resurrect. A foreign line is not evidence our plugin is installed; count it
    // as presence and self_heal's adopt row reinstalls the whole translation over a
    // plugin the user deliberately removed.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored after uninstall: {out}");
    assert_eq!(out, "NoOp", "a foreign status line resurrected an uninstalled plugin, got {out}");
    assert_eq!(status_line_text(&env.settings), seeded_text, "self-heal retook the slot after uninstall");
    assert!(env.marker().is_none(), "self-heal adopted a plugin that is not installed");
}

#[test]
fn antigravity_cli_statusline_stash_survives_a_declaration_change() {
    // Ownership decided by whole-value equality would read our OWN previous rendering
    // as "the user's original" the moment a release changes what we render, destroying
    // their value and poisoning the stash with our own command — which `compose` would
    // then run from inside itself, every turn. Only observable across a version change.
    let env = Env::new("statusline-version-change");
    seed_settings(&env);
    let seeded_text = status_line_text(&env.settings);

    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // What host version N left in the slot: our command, a padding this version no
    // longer renders.
    set_status_line_at(&env.settings, json!({"type": "command", "command": OUR_COMMAND, "padding": 1}));

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored on our own drifted rendering: {out}");
    assert_eq!(out, "Installed", "our own drifted rendering is drift to repair, got {out}");
    assert_eq!(status_line_at(&env.settings), our_status_line(), "self-heal did not converge our own rendering");

    // Asserted before the stash: the anti-recursion positive control. Break ownership
    // and the stash holds OUR command, so this call re-enters the binary.
    let (ok, out) = env.fixture_stdin(&["statusline", "--client", "antigravity-cli"], SESSION_JSON);
    assert!(ok, "statusline subcommand failed: {out}");
    assert_eq!(out, "ez-fixture row\ntheir-bar-row", "compose lost the user's row");
    assert_eq!(env.stashed_original(), seed_status_line(), "our own earlier rendering was stashed as the user's original");

    // The stash must survive an `update` between install and uninstall too.
    let (ok, out) = env.fixture(&["update"]);
    assert!(ok, "update failed: {out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(status_line_text(&env.settings), seeded_text, "the stash did not survive the version change plus update");
}

#[test]
fn antigravity_cli_statusline_deleted_line_is_readded_without_losing_the_stash() {
    let env = Env::new("statusline-drift");
    seed_settings(&env);
    let seeded_text = status_line_text(&env.settings);

    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    remove_status_line_at(&env.settings);
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal errored on a deleted statusLine: {out}");
    assert_eq!(out, "Installed", "a missing statusLine behind healthy surfaces is drift, got {out}");
    assert_eq!(status_line_at(&env.settings), our_status_line(), "self-heal did not re-add our statusLine");
    assert_eq!(env.stashed_original(), seed_status_line(), "the repair pass overwrote the user's stashed original");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(status_line_text(&env.settings), seeded_text, "uninstall did not restore the user's statusLine");
}

#[test]
fn antigravity_cli_statusline_is_user_scope_only() {
    // The 1.1.6 binary neither read nor rewrote a project-scope settings.json while
    // normalizing the user-scope one, so a project slot write would be inert config
    // dropped in someone's repo. The scope gate is what keeps us out of it — and the
    // project teardown path still reaches `forget`, which must no-op rather than
    // resolve a user-scope path and undo a DIFFERENT scope's install.
    let env = Env::new("statusline-project-scope");
    seed_settings(&env);
    let seeded_text = status_line_text(&env.settings);
    let project = env.root.join("project");
    fs::create_dir_all(&project).unwrap();
    let project_str = project.display().to_string();

    let (ok, out) = env.fixture(&["setup", "--agent", "antigravity-cli", "--project", &project_str]);
    assert!(ok, "project-scope setup failed: {out}");
    assert!(!project.join(".gemini").exists(), "a project-scope install wrote the CLI's own settings tree");
    assert_eq!(status_line_text(&env.settings), seeded_text, "a project-scope install wrote the USER's status line");

    let (ok, out) = env.fixture(&["uninstall", "--project", &project_str]);
    assert!(ok, "project-scope uninstall failed: {out}");
    assert_eq!(status_line_text(&env.settings), seeded_text, "a project-scope teardown undid the user-scope slot");
}

#[test]
fn antigravity_cli_teardown_with_nothing_installed_writes_nothing() {
    // A teardown that owns nothing in a user's file must not write to it at all.
    let env = Env::new("statusline-empty-teardown");
    seed_settings(&env);
    let before = fs::read_to_string(&env.settings).unwrap();

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall errored with nothing installed: {out}");
    assert_eq!(out, "NoOp", "a teardown with nothing of ours to undo must report no change, got {out}");
    assert_eq!(fs::read_to_string(&env.settings).unwrap(), before, "teardown wrote into a settings file it had nothing to undo in");
}
