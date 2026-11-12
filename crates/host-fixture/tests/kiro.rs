//! Hermetic kiro-backend lifecycle, fully isolated from a real `~/.kiro`. No
//! docker, no auth, no `kiro-cli`: the backend only ever writes kiro's config
//! files, so we drive `host_fixture setup --agent kiro` against a throwaway
//! `KIRO_HOME` and assert the written `settings/mcp.json` + `agents/default.json`
//! by parsing them back. `detect()` rides off the pre-created config dir.
//!
//! We set `KIRO_HOME` (kiro's documented base override) at the temp root, so every
//! path the backend touches derives from it — proving our writes land under that
//! root (and the seeded user entries survive) also proves it never reaches the
//! developer's real home. The resolution tests vary or drop that override
//! deliberately (empty/unset/set arms); the lifecycle tests keep it set. Hooks are
//! the interesting case: kiro nests them inside an
//! agent's own file, so we pre-seed `agents/default.json` (a real user agent) and
//! check our hooks merge in without disturbing the user's identity or their own hook.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our install
/// and uninstall untouched.
const SEED_MCP: &str = r#"{
  "someOtherSetting": true,
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  }
}
"#;

/// A real user default agent: identity fields + a hook of their own under an event
/// our fixture never emits, so it must survive our merge and our removal verbatim.
const SEED_AGENT: &str = r#"{
  "name": "default",
  "description": "the user's default agent",
  "prompt": "you are helpful",
  "hooks": {
    "stop": [ { "command": "user-own-stop-hook" } ]
  }
}
"#;

struct Env {
    root: PathBuf,
    kiro: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("kiro-cli")` (and every
    /// other backend's PATH probe) stays false and detection rides on `KIRO_HOME`.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        Self::seeded(name, ".kiro")
    }

    /// `kiro_home` is where `KIRO_HOME` points, relative to the temp root. The
    /// user-scope tests keep it at `.kiro` so that dropping `KIRO_HOME` resolves to
    /// the same place; the set-value test moves it elsewhere precisely so a resolver
    /// that ignored the override fails detect there.
    fn seeded(name: &str, kiro_home: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-kiro-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            kiro: root.join(kiro_home),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create the config dir so detect() passes with no `kiro-cli` on PATH, and
        // seed the mcp config + a real default agent the lifecycle must preserve.
        fs::create_dir_all(env.kiro.join("settings")).unwrap();
        fs::create_dir_all(env.kiro.join("agents")).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.kiro.join("settings").join("mcp.json"), SEED_MCP).unwrap();
        fs::write(env.kiro.join("agents").join("default.json"), SEED_AGENT).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("KIRO_HOME", &self.kiro)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("PATH", &self.path)
            .env_remove("AGENTGEAR_FIXTURE_MCP_ENV");
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// `fixture`, but run from `cwd` with `overrides` layered onto the sandbox env
    /// after `apply` (a later `env()` call replaces the earlier one), for the
    /// resolution variants a plain run cannot express — an empty override value,
    /// notably.
    fn fixture_with(&self, args: &[&str], cwd: &Path, overrides: &[(&str, &str)]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args).current_dir(cwd);
        self.apply(&mut cmd);
        for (key, value) in overrides {
            cmd.env(key, value);
        }
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// `fixture`, but run from `cwd` with the named vars REMOVED from the sandbox
    /// env — the resolution variant `Command::env` cannot express (an unset
    /// override), for the arm `Env::apply` always pre-sets.
    fn fixture_without(&self, args: &[&str], cwd: &Path, unset: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args).current_dir(cwd);
        self.apply(&mut cmd);
        for key in unset {
            cmd.env_remove(key);
        }
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn mcp(&self) -> String {
        fs::read_to_string(self.kiro.join("settings").join("mcp.json")).unwrap()
    }

    fn agent(&self) -> String {
        fs::read_to_string(self.kiro.join("agents").join("default.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `kiro-cli`, no sibling agent
/// CLIs, so the fan-out stays a pure kiro exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn kiro_full_lifecycle() {
    let env = Env::new("lifecycle");
    let skill_dir = env.kiro.join("skills").join("ez-skill");
    let skill = skill_dir.join("SKILL.md");

    // install: translates mcp into settings/mcp.json + skills (hooks: no kiro surface).
    let (ok, out) = env.fixture(&["setup", "--agent", "kiro"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let m = env.mcp();
    // our mcp server landed under `mcpServers`, Plain shape.
    assert!(m.contains("ez-fixture"), "our mcp server key missing:\n{m}");
    assert!(m.contains("host_fixture"), "our mcp command missing:\n{m}");
    // the seeded user config survived our merge.
    assert!(m.contains("theirs") && m.contains("their-server"), "seeded mcp server was clobbered:\n{m}");
    assert!(m.contains("someOtherSetting"), "seeded top-level key was clobbered:\n{m}");

    // remote mcp: both arms land in kiro's exact accepted shape.
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

    // hooks: capability is false — kiro's only hook surface is a user-owned
    // per-agent config json (its default agent is a setting, not a file named
    // default.json), so the backend must leave the seeded agent file untouched.
    let a = env.agent();
    assert_eq!(a, SEED_AGENT, "kiro backend must not touch the user's agent file:\n{a}");

    // skills: bare `<name>/SKILL.md` under ~/.kiro/skills, name+description ensured, tagged.
    assert!(skill.exists(), "skill SKILL.md not written: {}", skill.display());
    let sk = fs::read_to_string(&skill).unwrap();
    assert!(sk.contains("name: ez-skill") && sk.contains("description:"), "skill frontmatter missing name/description:\n{sk}");
    assert!(sk.contains("x-agentgear") && sk.contains("ez-fixture-plugin"), "ownership tag missing:\n{sk}");
    assert!(skill_dir.join("reference.md").exists(), "skill support file not copied through");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.kiro.join("settings").join("mcp.json"), env.kiro.join("agents").join("default.json"), skill.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "kiro"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let m = env.mcp();
    assert!(!m.contains("ez-fixture"), "our mcp server survived uninstall:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "uninstall removed the seeded mcp server:\n{m}");
    assert!(m.contains("someOtherSetting"), "uninstall removed the seeded top-level key:\n{m}");

    let a = env.agent();
    assert_eq!(a, SEED_AGENT, "uninstall must leave the user's agent file untouched:\n{a}");
    assert!(!skill_dir.exists(), "our skill dir survived uninstall: {}", skill_dir.display());

    // the post-uninstall config still parses: a clean re-install lands again.
    let (ok, out) = env.fixture(&["setup", "--agent", "kiro"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp().contains("ez-fixture"), "re-install did not re-add our server");
}

// --- instructions ------------------------------------------------------------

const INSTR_ID: &str = "ez-fixture-plugin@ez-fixture-plugin";
const INSTR_GUIDANCE: &str = "ez-fixture always-loaded guidance line.\n";
/// Our one steering file in `~/.kiro/steering/`; the plugin prefix keeps it
/// unambiguous beside the steering documents a user drops there themselves.
const INSTR_FILE: &str = "agentgear-ez-fixture-plugin.md";

fn instr_block() -> String {
    format!("<!-- agentgear:{INSTR_ID} begin -->\n{INSTR_GUIDANCE}<!-- agentgear:{INSTR_ID} end -->\n")
}

/// The user-scope instructions lifecycle: kiro's built-in agent loads every
/// `~/.kiro/steering/**/*.md` unconditionally, html comments intact and no frontmatter
/// required, so the surface is one plugin-named file holding our tagged block. Install
/// creates it; a drifted block is repaired by self-heal; uninstall removes exactly our
/// file, leaving a user's own steering document beside it untouched.
#[test]
fn kiro_instructions_steering_file_lifecycle() {
    let env = Env::new("instr-lifecycle");
    let steering = env.kiro.join("steering");
    let ours = steering.join(INSTR_FILE);

    let (ok, out) = env.fixture(&["setup", "--agent", "kiro"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");
    assert_eq!(fs::read_to_string(&ours).unwrap(), instr_block(), "install must write exactly our tagged block");

    // The Ok line pins the `instructions.is_some()` gate, the push and the user-scope path it is handed.
    let (ok, out) = env.fixture(&["doctor"]);
    assert!(ok, "doctor failed over a healthy install: {out}");
    let want = format!("[ ok ] guidance block present: block present in {}", ours.display());
    assert!(out.lines().any(|l| l == want), "the doctor report lacks an ok guidance-block check over a healthy install:\n{out}");

    let (ok, out) = env.fixture(&["setup", "--agent", "kiro"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");
    assert_eq!(fs::read_to_string(&ours).unwrap(), instr_block(), "a no-op setup must not rewrite the steering file");

    let tampered = format!("<!-- agentgear:{INSTR_ID} begin -->\ntampered\n<!-- agentgear:{INSTR_ID} end -->\n");
    fs::write(&ours, &tampered).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the drifted block: {out}");
    assert_eq!(fs::read_to_string(&ours).unwrap(), instr_block(), "self-heal must restore the exact block bytes");

    fs::remove_file(&ours).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the deleted steering file: {out}");
    assert_eq!(fs::read_to_string(&ours).unwrap(), instr_block(), "self-heal must re-create our steering file");

    let theirs = steering.join("their-own-steering.md");
    fs::write(&theirs, "their steering body\n").unwrap();
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert!(!ours.exists(), "uninstall must remove our steering file");
    assert_eq!(fs::read_to_string(&theirs).unwrap(), "their steering body\n", "uninstall rewrote a user steering document");
}

/// Project scope has no instructions surface: kiro's project steering lives in the
/// user's own repo, so a `--project` install must write no steering file on either side.
#[test]
fn kiro_instructions_user_scope_only() {
    let env = Env::new("instr-user-scope-only");
    let proj = env.root.join("proj");

    let (ok, out) = env.fixture(&["setup", "--agent", "kiro", "--project", proj.to_str().unwrap()]);
    assert!(ok, "setup failed: {out}");
    assert!(!proj.join(".kiro").join("steering").exists(), "project scope must not create a steering dir");
    assert!(!env.kiro.join("steering").join(INSTR_FILE).exists(), "a project install must not touch the user's steering dir");
}

/// kiro resolves an empty `KIRO_HOME` like an unset one, to the HOME-derived
/// `~/.kiro` (live-proven 2026-11-09 against kiro-cli 3.1.0: the unset and empty
/// arms of `kiro-cli settings list` read the same seeded `$HOME/.kiro` marker and
/// rc 0, while a non-empty value relocated the read; auth-free — the login wall
/// hits the agent subcommands, not settings). The resolver must ignore the empty
/// value the same way, or every user-scope write lands wherever the host binary
/// happened to run while kiro reads its real config.
#[test]
fn kiro_empty_kiro_home_value_resolves_to_the_home_dir() {
    let env = Env::new("empty-kiro-home");

    let cwd = env.root.join("empty-kiro-home-cwd");
    fs::create_dir_all(&cwd).unwrap();
    // A foreign cwd mcp config turns a cwd-relative resolution into an observable
    // write: any rewrite of this seed means the resolver read the empty value as a
    // path relative to the cwd. A literal-'' join has no `.kiro` to insert (the
    // override points AT the .kiro-equivalent dir), so its write would land at
    // `<cwd>/settings/mcp.json` and miss this seed entirely — the home-side assert
    // is the net for that class; the seed catches the `<cwd>/.kiro/…` shape.
    let cwd_seed = "{\n  \"cwdOnly\": true\n}\n";
    let cwd_kiro = cwd.join(".kiro").join("settings");
    fs::create_dir_all(&cwd_kiro).unwrap();
    fs::write(cwd_kiro.join("mcp.json"), cwd_seed).unwrap();

    let (ok, out) = env.fixture_with(&["setup", "--agent", "kiro"], &cwd, &[("KIRO_HOME", "")]);
    assert!(ok, "setup with an empty KIRO_HOME failed: {out}");
    assert_eq!(out, "Installed", "setup with an empty KIRO_HOME should install, got {out}");

    let home_mcp = env.mcp();
    assert!(
        home_mcp.contains("ez-fixture"),
        "our server did not land under the HOME-derived ~/.kiro kiro itself reads under an empty value:\n{home_mcp}"
    );
    assert_eq!(
        fs::read_to_string(cwd_kiro.join("mcp.json")).unwrap(),
        cwd_seed,
        "an empty KIRO_HOME resolved cwd-relative: the cwd mcp config was written or rewritten"
    );
}

/// agentgear resolves an UNSET `KIRO_HOME` to the HOME-derived `~/.kiro`, the same
/// root kiro itself reads there (probe arm A, measured 2026-11-09 against kiro-cli
/// 3.1.0). Every other hermetic test pre-sets `KIRO_HOME` via `Env::apply`, so a
/// fallback regression (a wrong leaf join, a foreign var honored ahead of HOME)
/// ships green everywhere else in the file.
#[test]
fn kiro_unset_kiro_home_resolves_to_the_home_dir() {
    let env = Env::new("unset-kiro-home");
    let cwd = env.root.join("unset-kiro-home-cwd");
    fs::create_dir_all(&cwd).unwrap();

    let (ok, out) = env.fixture_without(&["setup", "--agent", "kiro"], &cwd, &["KIRO_HOME"]);
    assert!(ok, "setup with KIRO_HOME unset failed: {out}");
    assert_eq!(out, "Installed", "setup with KIRO_HOME unset should install, got {out}");

    let home_mcp = env.mcp();
    assert!(home_mcp.contains("ez-fixture"), "our server did not land under the HOME-derived ~/.kiro with KIRO_HOME unset:\n{home_mcp}");
}

/// A non-empty `KIRO_HOME` relocates the whole backend to that dir (probe arm C,
/// measured 2026-11-09 against kiro-cli 3.1.0: the set control relocated the read).
/// The sandbox seeds the override target away from the HOME-derived root, so a
/// resolver that ignored the override and read `$HOME/.kiro` would fail detect
/// here (its dir would not exist) — every other test's override points at the
/// HOME-derived root, where both readings agree.
#[test]
fn kiro_set_kiro_home_relocates_the_backend() {
    let env = Env::seeded("set-kiro-home", "kiro-custom");

    let (ok, out) = env.fixture(&["setup", "--agent", "kiro"]);
    assert!(ok, "setup with KIRO_HOME set failed: {out}");
    assert_eq!(out, "Installed", "setup with KIRO_HOME set should install, got {out}");

    let mcp = env.mcp();
    assert!(mcp.contains("ez-fixture"), "our server did not land under the set KIRO_HOME root:\n{mcp}");
    assert!(!env.root.join(".kiro").exists(), "a set KIRO_HOME must not create the HOME-derived ~/.kiro");
}
