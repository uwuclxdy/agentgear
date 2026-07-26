//! Hermetic kimi-backend lifecycle, fully isolated from the real `~/.kimi-code`. No
//! docker, no auth, no `kimi` binary: the backend only ever writes kimi's file config
//! (`mcp.json` for mcp, `config.toml` for hooks), so we drive `host_fixture setup
//! --agent kimi` against a temp `KIMI_CODE_HOME` (+ HOME/XDG dirs) and assert the
//! written files by reading them back. `detect()` passes off the pre-created
//! `KIMI_CODE_HOME` dir alone (no `kimi` on PATH — which would be ambiguous with the
//! legacy python `kimi-cli` anyway).
//!
//! `KIMI_CODE_HOME` deliberately points at `<root>/custom-kimi-home`, NOT the
//! `<HOME>/.kimi-code` fallback of `kimi_home_opt` — so a regression that dropped the
//! `KIMI_CODE_HOME` override would land every write on the fallback path (which nothing
//! creates or reads here) and fail, instead of aliasing back onto the same dir.
//!
//! Every path the backend touches derives from `KIMI_CODE_HOME`/`HOME`, which we
//! point at a throwaway temp root — so proving our files land under that root (and
//! the seeded user entries survive) also proves the backend never reaches the real
//! config. TOML round-trip validity is proven indirectly: a second `setup` must
//! re-read `config.toml` (via `toml_edit`) and report `NoOp`, and a post-uninstall
//! re-install must parse it again and report `Installed`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our install
/// and uninstall untouched. mcp lives in `mcp.json`, separate from the hook config.
const SEED_MCP: &str = r#"{
  "theme": "dark",
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  }
}
"#;

/// A foreign hook under an event we also write to (`SessionStart`, collides with
/// ours) plus one under an event the fixture never emits (`Stop`), a top-level key,
/// and a comment — all of which MUST outlive our install and uninstall. `config.toml`
/// is a file separate from `mcp.json`, so this seed is load-bearing for hook
/// never-clobber + comment/key-order preservation (the point of the `toml_edit` path).
const SEED_CONFIG: &str = r#"# the user's own kimi config
model = "kimi-k2"

[[hooks]]
event = "SessionStart"
command = "their-session-hook"

[[hooks]]
event = "Stop"
command = "their-stop-hook"
"#;

struct Env {
    root: PathBuf,
    /// `<root>/custom-kimi-home` — the kimi home, pointed at via `KIMI_CODE_HOME`.
    /// Deliberately not `<root>/.kimi-code` (the `HOME`-based fallback of
    /// `kimi_home_opt`), so dropping the override can't silently alias onto the fallback.
    kimi: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so no sibling agent CLI is found
    /// and detection rides on the `~/.kimi-code` dir alone.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-kimi-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            kimi: root.join("custom-kimi-home"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create the kimi home so detect() passes with no `kimi` on PATH, and seed
        // an unrelated user mcp + hook config the lifecycle must preserve.
        fs::create_dir_all(&env.kimi).unwrap();
        for dir in [&env.config, &env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.kimi.join("mcp.json"), SEED_MCP).unwrap();
        fs::write(env.kimi.join("config.toml"), SEED_CONFIG).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("KIMI_CODE_HOME", &self.kimi)
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

    fn mcp_json(&self) -> String {
        fs::read_to_string(self.kimi.join("mcp.json")).unwrap()
    }

    fn config_toml(&self) -> String {
        fs::read_to_string(self.kimi.join("config.toml")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `kimi`, no sibling agent CLIs,
/// so the fan-out stays a pure kimi exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn kimi_full_lifecycle() {
    let env = Env::new("lifecycle");
    let skill_dir = env.kimi.join("skills").join("ez-skill");
    let skill = skill_dir.join("SKILL.md");

    // install: translates mcp -> mcp.json + hooks -> config.toml.
    let (ok, out) = env.fixture(&["setup", "--agent", "kimi"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // A fresh, healthy install must self-heal to a true NoOp: probe reads every surface
    // reconcile just wrote and finds no drift. Guards against a probe/reconcile desync
    // (widened surface probe, or probe rendering from the wrong source) that would churn.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    // mcp: our server landed under `mcpServers`, Plain shape.
    let m = env.mcp_json();
    assert!(m.contains("ez-fixture"), "our mcp server key missing:\n{m}");
    assert!(m.contains("host_fixture"), "our mcp command missing:\n{m}");
    // the seeded user mcp config survived our merge.
    assert!(m.contains("theirs") && m.contains("their-server"), "seeded mcp server was clobbered:\n{m}");

    // remote mcp: kimi's discriminator is `transport` (`type` is stripped by the
    // non-strict schema and a bare `{url}` infers http, so a `type:"sse"` entry
    // silently downgrades to http). The render must carry kimi's own key.
    let parsed: serde_json::Value = serde_json::from_str(&m).unwrap();
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-http"],
        serde_json::json!({"url": "http://127.0.0.1:39621/mcp", "transport": "http"}),
        "http remote arm mismatch:\n{m}"
    );
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-sse"],
        serde_json::json!({"url": "http://127.0.0.1:39622/sse", "transport": "sse"}),
        "sse remote arm mismatch:\n{m}"
    );
    assert!(m.contains("\"theme\"") && m.contains("dark"), "seeded top-level key was clobbered:\n{m}");

    // hooks: CC event names pass through 1:1 as `[[hooks]]` tables in config.toml.
    let c = env.config_toml();
    assert!(c.contains("event = \"SessionStart\""), "SessionStart hook missing:\n{c}");
    assert!(c.contains("event = \"UserPromptSubmit\""), "UserPromptSubmit hook missing:\n{c}");
    assert!(c.contains("host_fixture self-heal"), "SessionStart hook command missing:\n{c}");
    assert!(c.contains("host_fixture check-restart"), "UserPromptSubmit hook command missing:\n{c}");
    // the seeded user config survived: colliding-event hook, untouched-event hook, key, comment.
    assert!(c.contains("their-session-hook"), "seeded SessionStart hook was clobbered:\n{c}");
    assert!(c.contains("their-stop-hook"), "seeded Stop hook was clobbered:\n{c}");
    assert!(c.contains("model = \"kimi-k2\""), "seeded top-level key was clobbered:\n{c}");
    assert!(c.contains("the user's own kimi config"), "seeded comment was dropped (naive re-serialize?):\n{c}");

    // skills: bare `<name>/SKILL.md` under ~/.kimi-code/skills, ownership-tagged, support file copied.
    assert!(skill.exists(), "skill SKILL.md not written: {}", skill.display());
    let sk = fs::read_to_string(&skill).unwrap();
    assert!(sk.contains("name: ez-skill") && sk.contains("description:"), "skill frontmatter missing:\n{sk}");
    assert!(sk.contains("x-agentgear") && sk.contains("ez-fixture-plugin"), "ownership tag missing:\n{sk}");
    assert!(skill_dir.join("reference.md").exists(), "skill support file not copied through");

    // safety: everything we wrote is under the throwaway temp root.
    for target in [env.kimi.join("mcp.json"), env.kimi.join("config.toml"), skill.clone()] {
        assert!(target.starts_with(&env.root), "backend wrote outside the temp root: {}", target.display());
    }

    // idempotent: a second identical reconcile re-parses both files and no-ops.
    let (ok, out) = env.fixture(&["setup", "--agent", "kimi"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let m = env.mcp_json();
    assert!(!m.contains("ez-fixture"), "our mcp server survived uninstall:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "uninstall removed the seeded mcp server:\n{m}");
    assert!(m.contains("\"theme\"") && m.contains("dark"), "uninstall removed the seeded top-level key:\n{m}");
    let c = env.config_toml();
    assert!(!c.contains("host_fixture self-heal") && !c.contains("host_fixture check-restart"), "our hooks survived uninstall:\n{c}");
    assert!(c.contains("their-session-hook"), "uninstall removed the seeded SessionStart hook:\n{c}");
    assert!(c.contains("their-stop-hook"), "uninstall removed the seeded Stop hook:\n{c}");
    assert!(c.contains("model = \"kimi-k2\""), "uninstall removed the seeded top-level key:\n{c}");
    assert!(c.contains("the user's own kimi config"), "uninstall dropped the seeded comment:\n{c}");
    assert!(!skill_dir.exists(), "our skill dir survived uninstall: {}", skill_dir.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit/toml_edit would error on an unparseable file).
    let (ok, out) = env.fixture(&["setup", "--agent", "kimi"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp_json().contains("ez-fixture"), "re-install did not re-add our server");
}

/// The same never-clobber guarantee on a DEDICATED skill root (`~/.kimi-code/skills`,
/// not the shared `.agents/skills`): a foreign skill at our plugin skill's name survives
/// install (reconcile skips it) and uninstall (remove deletes only our-tagged dirs),
/// proving the ownership guard is root-agnostic, not shared-root-specific.
#[test]
fn kimi_never_clobbers_a_foreign_skill() {
    for (label, seed) in [
        ("untagged", "---\nname: ez-skill\ndescription: the user's own\n---\nkeep me\n"),
        ("rival-tagged", "---\nname: ez-skill\ndescription: rival\nx-agentgear: \"rival-plugin@rival-mkt\"\n---\nrival body\n"),
    ] {
        let env = Env::new(&format!("foreign-{label}"));
        let dir = env.kimi.join("skills").join("ez-skill");
        let skill = dir.join("SKILL.md");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&skill, seed).unwrap();

        let (ok, out) = env.fixture(&["setup", "--agent", "kimi"]);
        assert!(ok, "setup failed ({label}): {out}");
        assert_eq!(fs::read_to_string(&skill).unwrap(), seed, "install clobbered the foreign skill ({label})");

        let (ok, out) = env.fixture(&["uninstall"]);
        assert!(ok, "uninstall failed ({label}): {out}");
        assert!(skill.exists(), "uninstall deleted the foreign skill dir ({label})");
        assert_eq!(fs::read_to_string(&skill).unwrap(), seed, "uninstall altered the foreign skill ({label})");
    }
}

/// The uninstall inverse at file level, over kimi's array-of-tables instead of
/// codex's table: a `config.toml` whose every entry was ours goes with them, instead
/// of surviving as the 0 bytes an emptied `[[hooks]]` renders to. The lifecycle test
/// pins the other direction (the seeded foreign hooks and `model` key keep the file),
/// so this seeds only a comment — which goes too, our own removal having emptied
/// every entry it could have belonged to.
#[test]
fn kimi_uninstall_takes_a_config_toml_holding_nothing_but_ours() {
    let env = Env::new("empty-config");
    let config = env.kimi.join("config.toml");
    // Overwrite Env::new's seed: `SEED_CONFIG`'s foreign hooks and `model` key are
    // exactly what must NOT be here for the uninstall to be able to take the file.
    fs::write(&config, "# my kimi config\n").unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "kimi"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert!(env.config_toml().contains("[[hooks]]"), "install did not write our hooks:\n{}", env.config_toml());

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert!(!config.exists(), "uninstall left a config.toml holding nothing but what it had just taken back");

    // Re-install from nothing lands again, so taking the file is not a one-way door.
    let (ok, out) = env.fixture(&["setup", "--agent", "kimi"]);
    assert!(ok && out == "Installed", "re-install after the file was taken should install, got {out}");
    assert!(env.config_toml().contains("[[hooks]]"), "re-install did not re-add our hooks");
}

/// The guard on the other side of the same arm: a `config.toml` the user is already
/// keeping empty of our entries holds nothing for us to take back, so the teardown
/// must not write — or delete — at all. A comment-only file parses to an empty root,
/// which is what a root-emptiness test alone would misread as ours.
#[test]
fn kimi_uninstall_leaves_a_config_toml_it_never_wrote_to() {
    let env = Env::new("foreign-config");
    let config = env.kimi.join("config.toml");

    let (ok, out) = env.fixture(&["setup", "--agent", "kimi"]);
    assert!(ok && out == "Installed", "setup failed: {out}");

    // Hand the config back with our hooks already gone: that state is the user's own.
    let user_owned = "# my kimi config\n";
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
fn kimi_uninstall_leaves_a_windows_saved_config_toml_untouched() {
    for (label, seed) in [("crlf", "# my kimi config\r\n".as_bytes()), ("bom", "\u{feff}# my kimi config\n".as_bytes())] {
        let env = Env::new(&format!("windows-{label}"));
        let config = env.kimi.join("config.toml");

        let (ok, out) = env.fixture(&["setup", "--agent", "kimi"]);
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
