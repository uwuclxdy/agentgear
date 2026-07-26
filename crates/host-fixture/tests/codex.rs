//! Hermetic codex-backend lifecycle, fully isolated from the real `~/.codex`. No
//! docker, no auth, no `codex` binary: the backend only ever writes codex's file
//! config, so we drive `host_fixture setup --agent codex` against a temp
//! `CODEX_HOME` (+ HOME/XDG dirs) and assert the written `config.toml` / `hooks.json`
//! / prompt / agent files by reading them back. `detect()` passes off the
//! pre-created codex-home dir alone (no `codex` on PATH).
//!
//! Every path the backend touches derives from `CODEX_HOME`/`HOME`, which we point
//! at a throwaway temp root — so proving our files land under that root (and the
//! seeded user entries survive) also proves the backend never reaches the real
//! config. TOML round-trip parse validity is proven indirectly: a second `setup`
//! must read the written config back (via `toml_edit`) and report `NoOp`, and a
//! post-uninstall re-install must parse it again and report `Installed`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key + a comment that MUST outlive
/// our install and uninstall untouched (comment/key-order preservation is the whole
/// point of the bespoke `toml_edit` path over a naive re-serialize).
const SEED_CONFIG: &str = r#"# the user's own codex config
model = "gpt-5.4"

[mcp_servers.theirs]
command = "their-server"
args = ["--flag"]
"#;

/// A foreign hook under an event we also write to (`SessionStart`, collides with
/// ours) plus one under an event we never touch (`Stop`), both of which MUST
/// outlive our install and uninstall. `hooks.json` is a file separate from
/// `config.toml`, so this seed is load-bearing for hook never-clobber.
const SEED_HOOKS: &str = r#"{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "their-session-hook" } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "command": "their-stop-hook" } ] }
    ]
  }
}
"#;

struct Env {
    root: PathBuf,
    /// `<root>/.codex` — the codex home, pointed at via `CODEX_HOME`.
    codex: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("codex")` (and every
    /// other backend's PATH probe) stays false and detection rides on the codex dir.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-codex-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            codex: root.join(".codex"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create the codex home so detect() passes with no `codex` on PATH, and
        // seed an unrelated user config + hooks the lifecycle must preserve.
        fs::create_dir_all(&env.codex).unwrap();
        for dir in [&env.config, &env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.codex.join("config.toml"), SEED_CONFIG).unwrap();
        fs::write(env.codex.join("hooks.json"), SEED_HOOKS).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("CODEX_HOME", &self.codex)
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

    fn config_toml(&self) -> String {
        fs::read_to_string(self.codex.join("config.toml")).unwrap()
    }

    fn hooks_json(&self) -> String {
        fs::read_to_string(self.codex.join("hooks.json")).unwrap()
    }

    /// The project root a `--project`-scoped run installs into: its config base is
    /// `<project>/.codex`, independent of `CODEX_HOME`.
    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    fn project_config_toml(&self) -> String {
        fs::read_to_string(self.project().join(".codex").join("config.toml")).unwrap_or_default()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `codex`, no sibling agent
/// CLIs, so the fan-out stays a pure codex exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

/// Slice out one `[mcp_servers.<name>]` table's body from `config.toml`, so an
/// absence check (no `command` key) doesn't false-positive on a sibling table.
fn mcp_table_body<'a>(toml: &'a str, header: &str) -> &'a str {
    let start = toml.find(header).unwrap_or_else(|| panic!("table {header} not found:\n{toml}"));
    let body = &toml[start + header.len()..];
    let end = body.find("\n[").unwrap_or(body.len());
    &body[..end]
}

#[test]
fn codex_full_lifecycle() {
    let env = Env::new("lifecycle");
    let prompt_file = env.codex.join("prompts").join("ez-fixture-plugin-hello.md");
    let agent_file = env.codex.join("agents").join("ez-fixture-plugin-ez-helper.toml");
    let hooks_file = env.codex.join("hooks.json");

    // install: translates mcp + hooks + commands + agents into codex's config.
    let (ok, out) = env.fixture(&["setup", "--agent", "codex"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // A fresh, healthy install must self-heal to a true NoOp: probe reads every surface
    // reconcile just wrote and finds no drift. Guards against a probe/reconcile desync
    // (widened surface probe, or probe rendering from the wrong source) that would churn.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    // mcp: our server landed as a `[mcp_servers.ez-fixture]` table.
    let c = env.config_toml();
    assert!(c.contains("[mcp_servers.ez-fixture]"), "our mcp table missing:\n{c}");
    assert!(c.contains("command = \"host_fixture\""), "our mcp command missing:\n{c}");
    assert!(c.contains("\"mcp\""), "our mcp args missing:\n{c}");
    // the seeded user config survived our merge: server, top-level key, and comment.
    assert!(c.contains("[mcp_servers.theirs]") && c.contains("their-server"), "seeded mcp server was clobbered:\n{c}");
    assert!(c.contains("model = \"gpt-5.4\""), "seeded top-level key was clobbered:\n{c}");
    assert!(c.contains("the user's own codex config"), "seeded comment was dropped (naive re-serialize?):\n{c}");

    // remote mcp: codex is url-keyed for both remote kinds (best-effort), no `command`.
    let http_body = mcp_table_body(&c, "[mcp_servers.ez-fixture-http]");
    assert!(http_body.contains("url = \"http://127.0.0.1:39621/mcp\""), "http remote url missing:\n{c}");
    assert!(!http_body.contains("command"), "http remote must not carry `command`:\n{c}");
    let sse_body = mcp_table_body(&c, "[mcp_servers.ez-fixture-sse]");
    assert!(sse_body.contains("url = \"http://127.0.0.1:39622/sse\""), "sse remote url missing:\n{c}");
    assert!(!sse_body.contains("command"), "sse remote must not carry `command`:\n{c}");

    // hooks: CC event names pass through 1:1 into hooks.json (inert until /hooks trust).
    assert!(hooks_file.exists(), "hooks.json not written: {}", hooks_file.display());
    let h = env.hooks_json();
    assert!(h.contains("SessionStart"), "SessionStart hook missing:\n{h}");
    assert!(h.contains("UserPromptSubmit"), "UserPromptSubmit hook missing:\n{h}");
    assert!(h.contains("host_fixture self-heal"), "SessionStart hook command missing:\n{h}");
    assert!(h.contains("host_fixture check-restart"), "UserPromptSubmit hook command missing:\n{h}");
    // the seeded user hooks survived: one under a colliding event, one we never touch.
    assert!(h.contains("their-session-hook"), "seeded SessionStart hook was clobbered:\n{h}");
    assert!(h.contains("their-stop-hook"), "seeded Stop hook was clobbered:\n{h}");

    // commands: copy-through markdown prompt, plugin-prefixed + flat.
    assert!(prompt_file.exists(), "prompt markdown not written: {}", prompt_file.display());
    let p = fs::read_to_string(&prompt_file).unwrap();
    assert!(p.contains("Say hello"), "prompt body missing:\n{p}");

    // agents: translated to codex subagent TOML, plugin-prefixed name, model dropped.
    assert!(agent_file.exists(), "agent TOML not written: {}", agent_file.display());
    let a = fs::read_to_string(&agent_file).unwrap();
    assert!(a.contains("name = \"ez-fixture-plugin-ez-helper\""), "agent name not plugin-prefixed:\n{a}");
    assert!(a.contains("developer_instructions"), "agent developer_instructions missing:\n{a}");
    assert!(a.contains("fixture subagent"), "agent description not translated:\n{a}");
    assert!(a.contains("fixture helper agent"), "agent body not translated:\n{a}");
    assert!(!a.contains("model"), "CC model alias leaked into the codex agent (no reliable map):\n{a}");

    // safety: everything we wrote is under the throwaway temp root.
    for target in [env.codex.join("config.toml"), hooks_file.clone(), prompt_file.clone(), agent_file.clone()] {
        assert!(target.starts_with(&env.root), "backend wrote outside the temp root: {}", target.display());
    }

    // idempotent: a second identical reconcile re-parses the config and no-ops.
    let (ok, out) = env.fixture(&["setup", "--agent", "codex"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let c = env.config_toml();
    assert!(!c.contains("ez-fixture"), "our mcp server survived uninstall:\n{c}");
    assert!(c.contains("[mcp_servers.theirs]") && c.contains("their-server"), "uninstall removed the seeded mcp server:\n{c}");
    assert!(c.contains("model = \"gpt-5.4\""), "uninstall removed the seeded top-level key:\n{c}");
    assert!(c.contains("the user's own codex config"), "uninstall dropped the seeded comment:\n{c}");
    let h = env.hooks_json();
    assert!(!h.contains("host_fixture self-heal") && !h.contains("host_fixture check-restart"), "our hooks survived uninstall:\n{h}");
    assert!(h.contains("their-session-hook"), "uninstall removed the seeded SessionStart hook:\n{h}");
    assert!(h.contains("their-stop-hook"), "uninstall removed the seeded Stop hook:\n{h}");
    assert!(!prompt_file.exists(), "prompt file survived uninstall: {}", prompt_file.display());
    assert!(!agent_file.exists(), "agent file survived uninstall: {}", agent_file.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (toml_edit would error on an unparseable config.toml).
    let (ok, out) = env.fixture(&["setup", "--agent", "codex"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.config_toml().contains("[mcp_servers.ez-fixture]"), "re-install did not re-add our server");
}

#[test]
fn codex_project_scope_mcp_is_isolated_from_user_scope() {
    // `codex_base(Scope::Project { path })` resolves to `<path>/.codex`, a live
    // surface (docs/harness/codex.md gotcha 6): a real codex session run inside a
    // trusted project directory loads `[mcp_servers]` straight out of that project's
    // own `.codex/config.toml`, while codex's own `mcp` management CLI never reads it.
    // That CLI blindness is what made the write look dead, so this pins it.
    let env = Env::new("project-mcp");
    let project = env.project();

    // install: project-scope setup only ever touches <project>/.codex.
    let (ok, out) = env.fixture(&["setup", "--agent", "codex", "--project", &project.display().to_string()]);
    assert!(ok, "project-scope setup failed: {out}");
    assert_eq!(out, "Installed", "first project-scope setup should install, got {out}");

    let pc = env.project_config_toml();
    let body = mcp_table_body(&pc, "[mcp_servers.ez-fixture]");
    assert!(body.contains("command = \"host_fixture\""), "project config missing our mcp command:\n{pc}");

    // scope isolation: the user-scope config seeded by Env::new must be untouched —
    // no our-server entry landed there, and the seeded content survives byte-for-byte.
    let uc = env.config_toml();
    assert!(!uc.contains("ez-fixture"), "project-scope install leaked our server into user-scope config:\n{uc}");
    assert_eq!(uc, SEED_CONFIG, "project-scope install modified the user-scope config.toml at all:\n{uc}");

    // remove: a project-scope uninstall takes the entry back out of the project file
    // only, leaving the (still-untouched) user-scope file alone.
    let (ok, out) = env.fixture(&["uninstall", "--project", &project.display().to_string()]);
    assert!(ok, "project-scope uninstall failed: {out}");
    assert_eq!(out, "Removed", "project-scope uninstall should remove our entry, got {out}");

    let pc = env.project_config_toml();
    assert!(!pc.contains("[mcp_servers.ez-fixture]"), "our mcp server survived project-scope uninstall:\n{pc}");
    assert_eq!(env.config_toml(), SEED_CONFIG, "project-scope uninstall touched the user-scope config.toml:\n{}", env.config_toml());
}

/// The uninstall inverse at file level: a `config.toml` whose every key was ours goes
/// with them, instead of surviving as the 0 bytes an emptied implicit `[mcp_servers]`
/// renders to. The lifecycle test above pins the other direction (a foreign server and
/// a top-level key beside ours keep the file), so this seeds only a comment — which
/// goes too, our own removal having emptied every key it could have belonged to.
#[test]
fn codex_uninstall_takes_a_config_toml_holding_nothing_but_ours() {
    let env = Env::new("empty-config");
    let config = env.codex.join("config.toml");
    // Overwrite Env::new's seed: `SEED_CONFIG`'s foreign server and `model` key are
    // exactly what must NOT be here for the uninstall to be able to take the file.
    fs::write(&config, "# my codex config\n").unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "codex"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert!(env.config_toml().contains("[mcp_servers.ez-fixture]"), "install did not write our server:\n{}", env.config_toml());

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert!(!config.exists(), "uninstall left a config.toml holding nothing but what it had just taken back");

    // Re-install from nothing lands again, so taking the file is not a one-way door.
    let (ok, out) = env.fixture(&["setup", "--agent", "codex"]);
    assert!(ok && out == "Installed", "re-install after the file was taken should install, got {out}");
    assert!(env.config_toml().contains("[mcp_servers.ez-fixture]"), "re-install did not re-add our server");
}

/// The guard on the other side of the same arm: a `config.toml` the user is already
/// keeping empty of our keys holds nothing for us to take back, so the teardown must
/// not write — or delete — at all. A comment-only file parses to an empty root, which
/// is what a root-emptiness test alone would misread as ours.
#[test]
fn codex_uninstall_leaves_a_config_toml_it_never_wrote_to() {
    let env = Env::new("foreign-config");
    let config = env.codex.join("config.toml");

    let (ok, out) = env.fixture(&["setup", "--agent", "codex"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // Hand the config back with our server already gone: that state is the user's own.
    let user_owned = "# my codex config\n";
    fs::write(&config, user_owned).unwrap();

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall over a user-emptied config failed: {out}");
    assert!(config.exists(), "uninstall took a comment-only config it never wrote to");
    assert_eq!(env.config_toml(), user_owned, "uninstall rewrote a config that held nothing of ours");
}

/// The blocker the delete gate exists to avoid: `toml_edit` normalizes CRLF decor to
/// LF and strips a BOM, so a comment-only `config.toml` saved by a Windows editor does
/// not round-trip through its own renderer. A file arm keyed on "the text changed"
/// would read that as our own doing and delete a config we never wrote a byte to.
/// Keyed on the container prune instead, both survive byte-for-byte.
///
/// Byte survival is the NO-OP path's property, not a line-ending policy: a write we
/// genuinely have to make renders the whole document, so it lands LF and BOM-free
/// whatever came in. What is pinned here is that a teardown taking nothing back makes
/// no write at all.
#[test]
fn codex_uninstall_leaves_a_windows_saved_config_toml_untouched() {
    for (label, seed) in [("crlf", "# my codex config\r\n".as_bytes()), ("bom", "\u{feff}# my codex config\n".as_bytes())] {
        let env = Env::new(&format!("windows-{label}"));
        let config = env.codex.join("config.toml");

        let (ok, out) = env.fixture(&["setup", "--agent", "codex"]);
        assert!(ok && out == "Installed", "setup failed ({label}): {out}");

        // Hand the config back the way the user's own editor would have saved it,
        // with nothing of ours left in it.
        fs::write(&config, seed).unwrap();

        let (ok, out) = env.fixture(&["uninstall"]);
        assert!(ok, "uninstall over a {label} config failed: {out}");
        assert!(config.exists(), "uninstall took a {label} comment-only config it never wrote to");
        assert_eq!(fs::read(&config).unwrap(), seed, "uninstall rewrote a {label} config that held nothing of ours");
    }
}

/// The install side of the same no-op arm, which nothing else in the suite reaches.
/// `toml_write`'s convergence test compares the document's own render against itself
/// rather than against the bytes on disk, so a `config.toml` already holding exactly
/// what we would write converges even when its line endings or BOM mean it never
/// round-trips. Without that, every `setup` on a Windows-saved config reports
/// `Installed` and rewrites the file, and self_heal's adopt row misreports with it.
///
/// Covers both TOML backends: the arm lives in the shared `confedit::toml_write`, and
/// kimi's hook reconcile reaches it through the same `toml_edit` wrapper.
#[test]
fn codex_second_setup_over_a_windows_saved_config_toml_is_a_noop() {
    for (label, reseed) in [
        ("crlf", (|s: &str| s.replace('\n', "\r\n")) as fn(&str) -> String),
        ("bom", (|s: &str| format!("\u{feff}{s}")) as fn(&str) -> String),
    ] {
        let env = Env::new(&format!("windows-noop-{label}"));
        let config = env.codex.join("config.toml");

        let (ok, out) = env.fixture(&["setup", "--agent", "codex"]);
        assert!(ok && out == "Installed", "setup failed ({label}): {out}");
        assert!(env.config_toml().contains("[mcp_servers.ez-fixture]"), "install did not write our server ({label})");

        // Re-save the CONVERGED config the way the user's editor would: our server is
        // still in it, byte-identical in meaning, different on disk.
        let windows_saved = reseed(&env.config_toml());
        fs::write(&config, &windows_saved).unwrap();

        let (ok, out) = env.fixture(&["setup", "--agent", "codex"]);
        assert!(ok, "second setup over a {label} config failed: {out}");
        assert_eq!(out, "NoOp", "a converged {label} config must report NoOp, not a rewrite");
        assert_eq!(fs::read_to_string(&config).unwrap(), windows_saved, "second setup rewrote a converged {label} config");
    }
}
