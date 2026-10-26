//! Hermetic goose-backend lifecycle, fully isolated from the real `~/.config/goose`
//! and `~/.agents`. No docker, no auth, no `goose` binary: the backend only ever
//! writes goose's file config, so we drive `host_fixture setup --agent goose`
//! against a temp `XDG_CONFIG_HOME`/`HOME` and assert the written `config.yaml`
//! (mcp extensions) + plugin-owned `hooks.json` by reading them back. `detect()`
//! passes off the pre-created `<config>/goose` dir alone (no `goose` on PATH).
//!
//! Every path the backend touches derives from `XDG_CONFIG_HOME`/`HOME`, which we
//! point at a throwaway temp root — so proving our files land under that root (and
//! the seeded user entries survive) also proves it never reaches the real config.
//! YAML round-trip validity is proven indirectly: a second `setup` re-parses the
//! written config and reports `NoOp`, and a post-uninstall re-install parses it
//! again and reports `Installed`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign extension (no `enabled` field, so the self-heal test's `enabled: true`
/// -> `false` edit is unambiguous) + an unrelated top-level key, both of which MUST
/// outlive our install and uninstall untouched.
const SEED_CONFIG: &str = "GOOSE_MODEL: gpt-x\nextensions:\n  theirs:\n    type: stdio\n    cmd: their-server\n";

struct Env {
    root: PathBuf,
    /// `<config>/goose` — the user-scope goose config dir, pre-created for detect().
    goose: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("goose")` (and every
    /// other backend's PATH probe) stays false and detection rides on the config dir.
    path: OsString,
    /// `GOOSE_PATH_ROOT` override. Unset by default; the path-root test sets it to
    /// prove the backend follows goose's own unconditional precedence over
    /// `XDG_CONFIG_HOME`/`HOME` for both the config file and the plugins/hooks dir.
    path_root: Option<PathBuf>,
}

impl Env {
    /// `name` must be unique per test: `std::process::id()` alone is constant across
    /// every test in this binary, so two tests sharing one root race on it under
    /// cargo's default parallel test threads. Mirrors `tests/codex.rs`/`opencode.rs`.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-goose-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let config = root.join("config");
        let env = Env {
            goose: config.join("goose"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            path_root: None,
            config,
            root,
        };
        // Pre-create <config>/goose so detect() passes with no `goose` on PATH, and
        // seed an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.goose).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.goose.join("config.yaml"), SEED_CONFIG).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("PATH", &self.path);
        match &self.path_root {
            // A stray GOOSE_PATH_ROOT on the dev box would relocate every other
            // test's paths too; clear it so those keep resolving XDG/HOME.
            Some(root) => cmd.env("GOOSE_PATH_ROOT", root),
            None => cmd.env_remove("GOOSE_PATH_ROOT"),
        };
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Like [`Env::fixture`], but runs from `dir`: for the probes whose resolution
    /// depends on the working directory (a relative path root).
    fn fixture_in(&self, dir: &Path, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        cmd.current_dir(dir);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn config_yaml(&self) -> String {
        fs::read_to_string(self.goose.join("config.yaml")).unwrap()
    }

    /// `<HOME>/.agents/plugins/ez-fixture-plugin/hooks/hooks.json` — the plugin-owned
    /// hooks file, keyed off `HOME` (Open Plugins spec dir), not XDG config.
    fn hooks_json(&self) -> PathBuf {
        self.root.join(".agents").join("plugins").join("ez-fixture-plugin").join("hooks").join("hooks.json")
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `goose`, no sibling agent CLIs,
/// so the fan-out stays a pure goose exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn goose_full_lifecycle() {
    let env = Env::new("lifecycle");
    let hooks_file = env.hooks_json();
    let plugin_dir = env.root.join(".agents").join("plugins").join("ez-fixture-plugin");
    let skill_dir = plugin_dir.join("skills").join("ez-skill");
    let skill = skill_dir.join("SKILL.md");

    // install: translates mcp (extensions) + hooks into goose's config.
    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // A fresh, healthy install must self-heal to a true NoOp: probe reads every surface
    // reconcile just wrote and finds no drift. Guards against a probe/reconcile desync
    // (widened surface probe, or probe rendering from the wrong source) that would churn.
    // (Separate from the disable test below, where Disabled masks a spurious drift.)
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    // mcp: our server landed under `extensions` with goose's own field names.
    let c = env.config_yaml();
    assert!(c.contains("ez-fixture"), "our extension key missing:\n{c}");
    assert!(c.contains("cmd: host_fixture"), "goose `cmd` field missing:\n{c}");
    assert!(c.contains("type: stdio"), "goose `type` field missing:\n{c}");
    assert!(c.contains("enabled: true"), "goose `enabled` field missing:\n{c}");
    // the seeded user config survived our merge.
    assert!(c.contains("theirs") && c.contains("their-server"), "seeded extension was clobbered:\n{c}");
    assert!(c.contains("GOOSE_MODEL"), "seeded top-level key was clobbered:\n{c}");

    // remote mcp: the http arm lands under `extensions` in goose's own
    // `streamable_http` shape; sse is skipped outright — goose deserializes an
    // `sse` extension, then refuses it at runtime ("migrate to streamable_http"),
    // leaving a permanently dead entry.
    assert!(c.contains("name: ez-fixture-http"), "http remote `name` field missing:\n{c}");
    assert!(c.contains("type: streamable_http"), "http remote `type: streamable_http` missing:\n{c}");
    assert!(c.contains("uri: http://127.0.0.1:39621/mcp"), "http remote `uri` field missing:\n{c}");
    assert!(c.contains("enabled: true"), "http remote `enabled: true` missing:\n{c}");
    assert!(c.contains("timeout: 300"), "http remote `timeout` (DEFAULT_TIMEOUT) missing:\n{c}");
    assert!(!c.contains("ez-fixture-sse"), "sse extension must be skipped (goose runtime-refuses sse):\n{c}");

    // hooks: CC event names pass through 1:1 into the plugin-owned hooks.json.
    assert!(hooks_file.exists(), "hooks.json not written: {}", hooks_file.display());
    let h = fs::read_to_string(&hooks_file).unwrap();
    assert!(h.contains("SessionStart") && h.contains("host_fixture self-heal"), "SessionStart hook missing:\n{h}");
    assert!(h.contains("UserPromptSubmit") && h.contains("host_fixture check-restart"), "UserPromptSubmit hook missing:\n{h}");

    // skills: inside the `~/.agents/plugins/<plugin>/skills` dir goose owns, ownership-tagged.
    assert!(skill.exists(), "skill SKILL.md not written: {}", skill.display());
    let sk = fs::read_to_string(&skill).unwrap();
    assert!(sk.contains("name: ez-skill") && sk.contains("description:"), "skill frontmatter missing:\n{sk}");
    assert!(sk.contains("x-agentgear") && sk.contains("ez-fixture-plugin"), "ownership tag missing:\n{sk}");
    assert!(skill_dir.join("reference.md").exists(), "skill support file not copied through");

    // safety: everything we wrote is under the throwaway temp root.
    for target in [env.goose.join("config.yaml"), hooks_file.clone()] {
        assert!(target.starts_with(&env.root), "backend wrote outside the temp root: {}", target.display());
    }

    // idempotent: a second identical reconcile re-parses the config and no-ops.
    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // mutation guard: delete our SKILL.md; self-heal must repair (skills Absent -> NeedsRepair),
    // not NoOp. A no-op skills probe would leave it missing.
    fs::remove_file(&skill).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the deleted skill: {out}");
    assert!(skill.exists() && fs::read_to_string(&skill).unwrap().contains("x-agentgear"), "self-heal did not restore the tagged skill");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let c = env.config_yaml();
    assert!(!c.contains("ez-fixture"), "our extension survived uninstall:\n{c}");
    assert!(c.contains("theirs") && c.contains("their-server"), "uninstall removed the seeded extension:\n{c}");
    assert!(c.contains("GOOSE_MODEL"), "uninstall removed the seeded top-level key:\n{c}");
    // the whole plugin-owned hooks dir is dropped (we own it entirely).
    assert!(!plugin_dir.exists(), "our plugin hooks dir survived uninstall: {}", plugin_dir.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (yaml_edit would error on an unparseable config.yaml).
    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.config_yaml().contains("ez-fixture"), "re-install did not re-add our extension");
}

/// goose keeps only an ABSOLUTE `GOOSE_PATH_ROOT` (`config/paths.rs::validated_path_root`
/// at v1.56.0) and falls back to XDG/HOME otherwise; the backend must agree, or a
/// relative root scatters config where goose never loads it. Runs from a scratch cwd so
/// a regression cannot write into the repo.
#[test]
fn goose_ignores_a_relative_goose_path_root() {
    let mut env = Env::new("path-root-relative");
    let cwd = env.root.join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    env.path_root = Some(PathBuf::from("relroot"));

    let (ok, out) = env.fixture_in(&cwd, &["setup", "--agent", "goose"]);
    assert!(ok, "setup failed: {out}");
    assert!(!cwd.join("relroot").exists(), "a relative GOOSE_PATH_ROOT must be ignored, never resolved against cwd");
    assert!(
        env.config_yaml().contains("ez-fixture"),
        "an ignored relative root must fall back to the XDG config dir:\n{}",
        env.config_yaml()
    );
}

/// The `extensions` mapping is ours to take back only when our own removal is what
/// emptied it. Phase 1 pins the drop end to end (install creates the mapping,
/// uninstall takes it); phase 2 pins the guard on the other side, where the user is
/// keeping an empty mapping of their own and the teardown must not write at all.
/// Neither phase takes the file: `GOOSE_MODEL` survives in both, so the root is never
/// empty. The file arm itself is
/// `goose_uninstall_takes_a_config_yaml_holding_nothing_but_ours`.
#[test]
fn goose_uninstall_prunes_only_the_extensions_mapping_it_emptied() {
    let env = Env::new("prune");
    let config = env.goose.join("config.yaml");
    // No foreign extension in this seed: the `extensions` mapping has to be one WE
    // create for the uninstall to be its exact inverse. `SEED_CONFIG`'s `theirs` is
    // the opposite case, pinned by the lifecycle test above.
    fs::write(&config, "GOOSE_MODEL: gpt-x\n").unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert!(env.config_yaml().contains("extensions:"), "install did not create the extensions mapping:\n{}", env.config_yaml());

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert_eq!(env.config_yaml(), "GOOSE_MODEL: gpt-x\n", "uninstall left a shell of the mapping it created");

    // Re-install, then hand the config back with the mapping emptied: that state is
    // the user's own, since nothing of ours is left in it to take.
    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok && out == "Installed", "re-install failed: {out}");
    let user_owned = "# my goose config\nGOOSE_MODEL: gpt-x\nextensions: {}\n";
    fs::write(&config, user_owned).unwrap();

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall over a user-emptied mapping failed: {out}");
    assert_eq!(env.config_yaml(), user_owned, "uninstall took a mapping the user was keeping empty");
}

/// goose's `extensions.<name>.enabled` is a real per-extension on/off flag a user
/// can flip by hand. self_heal must classify a user disable as `Disabled` and never
/// write it back to `true`; an explicit `setup` honors user intent and re-enables,
/// mirroring the claude backend's disabled-entry rule.
#[test]
fn goose_self_heal_never_reenables_a_user_disable() {
    let env = Env::new("self-heal-disable");

    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok && out == "Installed", "initial setup failed: {out}");

    // Simulate the user disabling our extension through goose's own `enabled` flag.
    // The seed's foreign extension has no `enabled` field, so this hits only ours.
    let disabled = env.config_yaml().replace("enabled: true", "enabled: false");
    assert!(disabled.contains("enabled: false"), "seed replace missed the field:\n{disabled}");
    fs::write(env.goose.join("config.yaml"), &disabled).unwrap();

    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok, "self-heal failed: {out}");
    assert_eq!(out, "NoOp", "self-heal must not touch a deliberately-disabled entry, got {out}");
    assert!(env.config_yaml().contains("enabled: false"), "self-heal re-enabled a user-disabled extension");

    // An explicit re-run of setup honors the user's request and re-enables.
    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok, "re-setup failed: {out}");
    assert_ne!(out, "NoOp", "explicit setup should have re-enabled the disabled entry, got {out}");
    assert!(env.config_yaml().contains("enabled: true"), "explicit setup did not re-enable the disabled entry");
}

/// `GOOSE_PATH_ROOT` wins unconditionally over `XDG_CONFIG_HOME`/`HOME` and relocates
/// both surfaces at once: goose resolves the config file at `<root>/config/config.yaml`
/// (not `<XDG_CONFIG_HOME>/goose/config.yaml`) and the plugins/hooks dir at
/// `<root>/.agents/plugins` (not the ordinary HOME-based `~/.agents/plugins`).
/// `detect()` does not consult the env (out of this fix's scope), so it still rides
/// the `<XDG_CONFIG_HOME>/goose` dir `Env::new` pre-creates.
#[test]
fn goose_honors_goose_path_root() {
    let mut env = Env::new("path-root");
    let path_root = env.root.join("pathroot");
    env.path_root = Some(path_root.clone());

    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok, "setup under GOOSE_PATH_ROOT failed: {out}");
    assert_eq!(out, "Installed", "first setup under GOOSE_PATH_ROOT should install, got {out}");

    // config.yaml lands at <root>/config/config.yaml, goose's own relocated layout.
    let relocated_config = path_root.join("config").join("config.yaml");
    assert!(relocated_config.is_file(), "config.yaml not written under GOOSE_PATH_ROOT: {}", relocated_config.display());
    let c = fs::read_to_string(&relocated_config).unwrap();
    assert!(c.contains("ez-fixture") && c.contains("cmd: host_fixture"), "our extension missing from the relocated config:\n{c}");

    // the XDG-path config.yaml (pre-seeded for detect()) must stay untouched: the
    // backend must never write the config goose would never read.
    let xdg_config = fs::read_to_string(env.goose.join("config.yaml")).unwrap();
    assert_eq!(xdg_config, SEED_CONFIG, "config.yaml at the XDG path must stay untouched when GOOSE_PATH_ROOT is set:\n{xdg_config}");

    // hooks.json lands at <root>/.agents/plugins/<plugin>/hooks/hooks.json.
    let relocated_hooks = path_root.join(".agents").join("plugins").join("ez-fixture-plugin").join("hooks").join("hooks.json");
    assert!(relocated_hooks.is_file(), "hooks.json not written under GOOSE_PATH_ROOT: {}", relocated_hooks.display());
    let h = fs::read_to_string(&relocated_hooks).unwrap();
    assert!(h.contains("SessionStart") && h.contains("host_fixture self-heal"), "SessionStart hook missing from the relocated hooks:\n{h}");

    // the ordinary HOME-based hooks dir must never be created: the backend must
    // never write a hooks dir goose would never scan.
    assert!(
        !env.hooks_json().exists(),
        "hooks.json must not land at the HOME path when GOOSE_PATH_ROOT is set: {}",
        env.hooks_json().display()
    );

    // .goosehints follows the same relocated config dir (goose derives it from
    // `Paths::config_dir()`): a block at the XDG path would be one goose never loads.
    let relocated_hints = path_root.join("config").join(".goosehints");
    assert!(relocated_hints.is_file(), ".goosehints not written under GOOSE_PATH_ROOT: {}", relocated_hints.display());
    let hints = fs::read_to_string(&relocated_hints).unwrap();
    assert!(hints.contains("agentgear:ez-fixture-plugin"), "our block missing from the relocated .goosehints:\n{hints}");
    assert!(!env.goose.join(".goosehints").exists(), "the XDG-path .goosehints must stay unwritten when GOOSE_PATH_ROOT is set");

    // idempotent: a second identical reconcile under the same root no-ops.
    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok && out == "NoOp", "second setup under GOOSE_PATH_ROOT should no-op, got {out}");

    // uninstall follows the same relocated paths and cleans them up.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall under GOOSE_PATH_ROOT failed: {out}");
    // The relocated config held nothing but our extension, so it goes with it.
    assert!(!relocated_config.exists(), "our extension survived uninstall under GOOSE_PATH_ROOT: {}", relocated_config.display());
    assert!(!relocated_hints.exists(), "our block survived uninstall under GOOSE_PATH_ROOT: {}", relocated_hints.display());
    let relocated_plugin_dir = path_root.join(".agents").join("plugins").join("ez-fixture-plugin");
    assert!(!relocated_plugin_dir.exists(), "the relocated plugin hooks dir survived uninstall: {}", relocated_plugin_dir.display());
}

// --- instructions ------------------------------------------------------------

/// The fixture's `<name>@<marketplace>` plugin id; the derive defaults `marketplace`
/// to `name` when no attr names it.
const INSTR_ID: &str = "ez-fixture-plugin@ez-fixture-plugin";
const INSTR_GUIDANCE: &str = "ez-fixture always-loaded guidance line.\n";

/// The exact tagged block the instructions surface must render into the global
/// `<config-dir>/.goosehints`: the host guidance between two html-comment markers
/// naming the plugin id, the whole block replaceable byte-for-byte.
fn instr_block() -> String {
    format!("<!-- agentgear:{INSTR_ID} begin -->\n{INSTR_GUIDANCE}<!-- agentgear:{INSTR_ID} end -->\n")
}

/// The user-scope instructions lifecycle in goose's global `<config-dir>/.goosehints`
/// (`hints/load_hints.rs` builds the file list from `Paths::in_config_dir(name)` over
/// the `CONTEXT_FILE_NAMES` defaults `[.goosehints, AGENTS.md]`, and prompt assembly
/// loads every entry unconditionally): install appends our block after the user's
/// content with one blank separator line; a drifted or deleted block is repaired by
/// self-heal; uninstall strips exactly our block and the separator.
#[test]
fn goose_instructions_block_lifecycle() {
    let env = Env::new("instr-lifecycle");
    let hints = env.goose.join(".goosehints");
    let seed = "user hint one\nuser hint two\n";
    fs::write(&hints, seed).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");
    let installed = format!("{seed}\n{}", instr_block());
    assert_eq!(fs::read_to_string(&hints).unwrap(), installed, "install must append exactly our block after a blank line");

    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");
    assert_eq!(fs::read_to_string(&hints).unwrap(), installed, "a no-op setup must not rewrite .goosehints");

    let tampered = format!("{seed}\n<!-- agentgear:{INSTR_ID} begin -->\ntampered\n<!-- agentgear:{INSTR_ID} end -->\n");
    fs::write(&hints, &tampered).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the drifted block: {out}");
    assert_eq!(fs::read_to_string(&hints).unwrap(), installed, "self-heal must restore the exact block bytes");

    fs::write(&hints, seed).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the deleted block: {out}");
    assert_eq!(fs::read_to_string(&hints).unwrap(), installed, "self-heal must re-add the block");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(fs::read_to_string(&hints).unwrap(), seed, "uninstall must restore the user's file byte-for-byte");
}

/// With no pre-existing `.goosehints`, install creates the file holding only our
/// block, and uninstall — our removal being what emptied it — takes the file with it.
#[test]
fn goose_instructions_owns_only_its_file() {
    let env = Env::new("instr-owned-file");
    let hints = env.goose.join(".goosehints");
    assert!(!hints.exists(), "precondition: no .goosehints seeded");

    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(fs::read_to_string(&hints).unwrap(), instr_block(), "a file we created holds exactly our block, no leading blank line");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert!(!hints.exists(), "uninstall must remove the file our removal emptied");
}

/// A rival agentgear host's block (different plugin id in the markers) and the user's
/// own lines must survive our install and uninstall byte-identical: the marker id
/// scopes ownership, so two agentgear plugins coexist in one `.goosehints`.
#[test]
fn goose_never_clobbers_a_rival_instructions_block() {
    let env = Env::new("instr-rival");
    let hints = env.goose.join(".goosehints");
    let rival =
        "user line\n<!-- agentgear:rival-plugin@rival-mkt begin -->\nrival guidance\n<!-- agentgear:rival-plugin@rival-mkt end -->\n";
    fs::write(&hints, rival).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(
        fs::read_to_string(&hints).unwrap(),
        format!("{rival}\n{}", instr_block()),
        "install must append our block, leaving the rival's untouched"
    );

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(fs::read_to_string(&hints).unwrap(), rival, "uninstall must strip only our block, keeping the rival's and the user's");
}

/// goose also loads `<config-dir>/AGENTS.md` and `~/.agents/AGENTS.md`, but both are
/// files the user and every other AGENTS.md reader own — never a surface we write.
/// After a full install the block exists in `.goosehints` alone.
#[test]
fn goose_instructions_leaves_shared_agents_md_unwritten() {
    let env = Env::new("instr-shared-agents-md");

    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok, "setup failed: {out}");
    assert!(env.goose.join(".goosehints").exists(), "precondition: our block landed");

    let config_agents = env.goose.join("AGENTS.md");
    let shared_agents = env.root.join(".agents").join("AGENTS.md");
    assert!(!config_agents.exists(), "the config-dir AGENTS.md is a shared context file, never ours to write: {}", config_agents.display());
    assert!(!shared_agents.exists(), "the cross-tool ~/.agents/AGENTS.md is never ours to write: {}", shared_agents.display());
}

/// The uninstall inverse at file level, the arm
/// `goose_uninstall_prunes_only_the_extensions_mapping_it_emptied` stops one level
/// short of: a `config.yaml` whose every key was ours goes with the `extensions`
/// mapping. That test's seed keeps `GOOSE_MODEL`, so its file must survive; this one
/// seeds only a comment, which goes too — our own removal having emptied every key it
/// could have belonged to.
#[test]
fn goose_uninstall_takes_a_config_yaml_holding_nothing_but_ours() {
    let env = Env::new("empty-config");
    let config = env.goose.join("config.yaml");
    // Overwrite Env::new's seed: `SEED_CONFIG`'s foreign extension and top-level keys
    // are exactly what must NOT be here for the uninstall to be able to take the file.
    fs::write(&config, "# my goose config\n").unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok && out == "Installed", "setup failed: {out}");
    assert!(env.config_yaml().contains("extensions:"), "install did not create the extensions mapping:\n{}", env.config_yaml());

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");
    assert!(!config.exists(), "uninstall left a config.yaml holding nothing but what it had just taken back");

    // Re-install from nothing lands again, so taking the file is not a one-way door.
    let (ok, out) = env.fixture(&["setup", "--agent", "goose"]);
    assert!(ok && out == "Installed", "re-install after the file was taken should install, got {out}");
    assert!(env.config_yaml().contains("ez-fixture"), "re-install did not re-add our extension");
}
