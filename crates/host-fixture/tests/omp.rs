//! Hermetic omp-backend lifecycle, fully isolated from the real `~/.omp`. No docker,
//! no auth, no `omp` binary: the backend only ever writes omp's OMP-native config
//! files, so we drive `host_fixture setup --agent omp` against a temp `HOME` and
//! assert the written `mcp.json` / command / agent files by reading them back.
//! `detect()` passes off the pre-created `~/.omp` dir alone.
//!
//! Every path the backend touches derives from `HOME` (user scope = `~/.omp/agent`),
//! which we point at a throwaway temp root — so proving our files land under that
//! root (and the seeded user entries survive) also proves the backend never reaches
//! the developer's real home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key (`$schema`, a real omp key) that
/// MUST outlive our install and uninstall untouched.
const SEED_MCP: &str = r#"{
  "$schema": "https://x/mcp-schema.json",
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  }
}
"#;

struct Env {
    root: PathBuf,
    /// `~/.omp/agent` — the user-scope surface base holding mcp.json/commands/agents.
    base: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("omp")` (and every other
    /// backend's PATH probe) stays false and detection rides on `~/.omp`.
    path: OsString,
}

impl Env {
    fn new(name: &str) -> Self {
        // `name` disambiguates the temp root: `process::id()` is constant across every
        // test in this binary, so a second test would otherwise share (and wipe) this one.
        let root = std::env::temp_dir().join(format!("ez-omp-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            base: root.join(".omp").join("agent"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            root,
        };
        // Pre-create ~/.omp/agent so detect() passes with no `omp` on PATH, and seed
        // an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(&env.base).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(env.base.join("mcp.json"), SEED_MCP).unwrap();
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("PATH", &self.path)
            // A stray PI_CONFIG_DIR/profile on the dev box would relocate the config
            // root; clear them so the backend resolves the default `~/.omp/agent`.
            .env_remove("PI_CONFIG_DIR")
            .env_remove("PI_CODING_AGENT_DIR")
            .env_remove("OMP_PROFILE")
            .env_remove("PI_PROFILE");
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn mcp(&self) -> String {
        fs::read_to_string(self.base.join("mcp.json")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `omp`, no sibling agent CLIs,
/// so the fan-out stays a pure omp exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

/// Seed Claude Code's on-disk plugin registry under `claude_dir` (a `.../.claude`): the
/// `installed_plugins.json` omp's `claude-plugins` provider reads (numeric `version` key
/// mandatory). This is the sole CC-side signal the retire gate consults — CC's own
/// `enabledPlugins` is irrelevant, because omp surfaces the agent off this file regardless
/// of CC's enable state. The plugin id matches the fixture: `<name>@<marketplace>`, both
/// `ez-fixture-plugin`.
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

/// Write omp's global config (`~/.omp/agent/config.yml`) disabling the `claude-plugins`
/// provider, so `isProviderEnabled("claude-plugins")` reads false — omp then does NOT
/// surface CC agents and our translation must run. `env.base` is `~/.omp/agent`.
fn disable_omp_claude_plugins_provider(env: &Env) {
    fs::write(env.base.join("config.yml"), "disabledProviders:\n  - claude-plugins\n").unwrap();
}

/// The user-scope agent file the fixture's `ez-helper` agent def translates to.
fn agent_file(env: &Env) -> PathBuf {
    env.base.join("agents").join("ez-fixture-plugin-ez-helper.md")
}

#[test]
fn omp_retires_agents_when_cc_registry_covers() {
    // CC's registry lists the plugin and omp's `claude-plugins` provider is on (default,
    // no omp config disabling it), so omp already surfaces the agents off
    // `$HOME/.claude/plugins/installed_plugins.json` — translating them ourselves would
    // double-register. The retire skips only the agent write; mcp + commands still land.
    let env = Env::new("retire");
    seed_cc_registry(&env.root.join(".claude"));

    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "retired install still writes mcp + commands, so it installs; got {out}");

    // agents retired: no agent file written despite the plugin having an agent def.
    assert!(!agent_file(&env).exists(), "agent file written despite CC-registry coverage: {}", agent_file(&env).display());
    // mcp + commands still translate.
    assert!(env.mcp().contains("ez-fixture"), "mcp not written on the retired path:\n{}", env.mcp());
    assert!(env.base.join("commands").join("ez-fixture-plugin-hello.md").exists(), "command file not written on the retired path");

    // self_heal is a true NoOp: the retired agents surface contributes `None` (nothing
    // owned), not `Absent`, so a healthy mcp+commands backend never churns to NeedsRepair.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal on a retired install should no-op (no churn), got {out}");

    // uninstall still cleans mcp + commands (and no-ops the never-written agents).
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall on a retired install failed: {out}");
    assert!(!env.mcp().contains("ez-fixture"), "our mcp server survived uninstall on the retired path");
}

#[test]
fn omp_translates_agents_when_cc_registry_relocated() {
    // A relocated `CLAUDE_CONFIG_DIR` moves CC's real registry off omp's HOME-based read
    // path (`$HOME/.claude`, `src/discovery/helpers.ts:895`). omp — and our gate — read
    // `$HOME/.claude`, which is empty here, so the agents MUST translate: an omp-only or
    // relocated-config install must never lose its agents. The registry seeded at the
    // relocated dir is the negative control proving the gate ignores `CLAUDE_CONFIG_DIR`.
    let env = Env::new("relocated");
    let alt = env.root.join("altcfg").join(".claude");
    seed_cc_registry(&alt);
    assert!(!env.root.join(".claude").exists(), "test setup error: HOME-based .claude must be absent");

    let mut cmd = Command::new(BIN);
    cmd.args(["setup", "--agent", "omp"]);
    env.apply(&mut cmd);
    cmd.env("CLAUDE_CONFIG_DIR", env.root.join("altcfg"));
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "setup failed: {}", String::from_utf8_lossy(&out.stdout));

    // Agents translated: the HOME-based gate never consulted the relocated registry.
    assert!(agent_file(&env).exists(), "agent file not written under a relocated CLAUDE_CONFIG_DIR: {}", agent_file(&env).display());
    let a = fs::read_to_string(agent_file(&env)).unwrap();
    assert!(a.contains("name: ez-fixture-plugin-ez-helper"), "translated agent missing its namespaced name:\n{a}");
}

#[test]
fn omp_translates_agents_when_provider_disabled_even_though_installed() {
    // The plugin IS listed in CC's registry, but omp's `claude-plugins` provider is
    // disabled in omp's own config (`disabledProviders: [claude-plugins]`). omp then does
    // NOT surface CC agents (`isProviderEnabled` false), so retiring would lose them — we
    // must translate. This is the branch CC's `enabledPlugins` could never capture.
    let env = Env::new("provider-off");
    seed_cc_registry(&env.root.join(".claude"));
    disable_omp_claude_plugins_provider(&env);

    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "install should still land, got {out}");

    assert!(
        agent_file(&env).exists(),
        "agent file not written despite the omp claude-plugins provider being disabled: {}",
        agent_file(&env).display()
    );

    // A fresh install with agents translated must still self-heal to a true NoOp.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a provider-disabled install should no-op, got {out}");
}

#[test]
fn omp_full_lifecycle() {
    let env = Env::new("lifecycle");
    let cmd_file = env.base.join("commands").join("ez-fixture-plugin-hello.md");
    let agent_file = env.base.join("agents").join("ez-fixture-plugin-ez-helper.md");

    // install: translates mcp + commands + agents into omp's config (no hooks surface).
    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // A fresh, healthy install must self-heal to a true NoOp: probe reads every surface
    // reconcile just wrote and finds no drift. Guards against a probe/reconcile desync
    // (widened surface probe, or probe rendering from the wrong source) that would churn.
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out == "NoOp", "self-heal after a fresh install should no-op, got {out}");

    let m = env.mcp();
    // our mcp server landed under `mcpServers`, Plain shape (no `type`, omp defaults stdio).
    assert!(m.contains("ez-fixture"), "our mcp server key missing:\n{m}");
    assert!(m.contains("host_fixture"), "our mcp command missing:\n{m}");
    // the seeded user config survived our merge.
    assert!(m.contains("theirs") && m.contains("their-server"), "seeded mcp server was clobbered:\n{m}");
    assert!(m.contains("$schema"), "seeded top-level key was clobbered:\n{m}");

    // remote mcp: both arms land in omp's exact accepted shape.
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

    // commands: the CC command copies through verbatim, plugin-prefixed + flat.
    assert!(cmd_file.exists(), "command file not written: {}", cmd_file.display());
    let c = fs::read_to_string(&cmd_file).unwrap();
    assert!(c.contains("Say hello"), "command body not copied through:\n{c}");
    assert!(c.contains("description:"), "command frontmatter description missing:\n{c}");

    // agents: re-emitted with a namespaced name, description, and the body as systemPrompt.
    assert!(agent_file.exists(), "agent file not written: {}", agent_file.display());
    let a = fs::read_to_string(&agent_file).unwrap();
    assert!(a.contains("name: ez-fixture-plugin-ez-helper"), "namespaced agent name missing:\n{a}");
    assert!(a.contains("description:"), "agent description missing:\n{a}");
    assert!(!a.contains("sonnet"), "CC model alias leaked into the omp agent:\n{a}");
    assert!(a.contains("fixture helper agent"), "agent body (systemPrompt) missing:\n{a}");

    // safety: everything we wrote is under the throwaway temp root.
    for p in [env.base.join("mcp.json"), cmd_file.clone(), agent_file.clone()] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let m = env.mcp();
    assert!(!m.contains("ez-fixture"), "our mcp server survived uninstall:\n{m}");
    assert!(m.contains("theirs") && m.contains("their-server"), "uninstall removed the seeded mcp server:\n{m}");
    assert!(m.contains("$schema"), "uninstall removed the seeded top-level key:\n{m}");
    assert!(!cmd_file.exists(), "our command file survived uninstall: {}", cmd_file.display());
    assert!(!agent_file.exists(), "our agent file survived uninstall: {}", agent_file.display());

    // the post-uninstall config still parses: a clean re-install lands again
    // (json_edit would error on an unparseable mcp.json).
    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.mcp().contains("ez-fixture"), "re-install did not re-add our server");
}

// --- instructions ------------------------------------------------------------

const INSTR_ID: &str = "ez-fixture-plugin@ez-fixture-plugin";
const INSTR_GUIDANCE: &str = "ez-fixture always-loaded guidance line.\n";

fn instr_block() -> String {
    format!("<!-- agentgear:{INSTR_ID} begin -->\n{INSTR_GUIDANCE}<!-- agentgear:{INSTR_ID} end -->\n")
}

/// The user-scope instructions lifecycle in omp's `~/.omp/agent/AGENTS.md` (the
/// `native` provider loads it into every request, raw text with html comments intact):
/// install appends our block after the user's content with one blank separator line;
/// a drifted or deleted block is repaired by self-heal; uninstall strips exactly our
/// block and the separator.
#[test]
fn omp_instructions_block_lifecycle() {
    let env = Env::new("instr-lifecycle");
    let agents = env.base.join("AGENTS.md");
    let seed = "user rule one\nuser rule two\n";
    fs::write(&agents, seed).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");
    let installed = format!("{seed}\n{}", instr_block());
    assert_eq!(fs::read_to_string(&agents).unwrap(), installed, "install must append exactly our block after a blank line");

    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), installed, "a no-op setup must not rewrite AGENTS.md");

    let tampered = format!("{seed}\n<!-- agentgear:{INSTR_ID} begin -->\ntampered\n<!-- agentgear:{INSTR_ID} end -->\n");
    fs::write(&agents, &tampered).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the drifted block: {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), installed, "self-heal must restore the exact block bytes");

    fs::write(&agents, seed).unwrap();
    let (ok, out) = env.fixture(&["self-heal"]);
    assert!(ok && out != "NoOp", "self-heal ignored the deleted block: {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), installed, "self-heal must re-add the block");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), seed, "uninstall must restore the user's file byte-for-byte");
}

/// With no pre-existing `AGENTS.md`, install creates the file holding only our block,
/// and uninstall — our removal being what emptied it — takes it with it.
#[test]
fn omp_instructions_owns_only_its_file() {
    let env = Env::new("instr-owned-file");
    let agents = env.base.join("AGENTS.md");
    assert!(!agents.exists(), "precondition: no AGENTS.md seeded");

    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), instr_block(), "a file we created holds exactly our block, no leading blank line");

    // The doctor arm is wired, not merely declared: a full report names our block.
    // Without this, dropping the check or inverting its `instructions.is_some()` gate
    // leaves the whole suite green.
    let (ok, out) = env.fixture(&["doctor"]);
    assert!(ok, "doctor failed over a healthy install: {out}");
    assert!(out.contains("guidance block present"), "the doctor report never checks the guidance block:\n{out}");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert!(!agents.exists(), "uninstall must remove the file our removal emptied");
}

/// A rival agentgear host's block (different plugin id in the markers) and the user's
/// own lines must survive our install and uninstall byte-identical: the marker id
/// scopes ownership, so two agentgear plugins coexist in one AGENTS.md.
#[test]
fn omp_never_clobbers_a_rival_instructions_block() {
    let env = Env::new("instr-rival");
    let agents = env.base.join("AGENTS.md");
    let rival =
        "user line\n<!-- agentgear:rival-plugin@rival-mkt begin -->\nrival guidance\n<!-- agentgear:rival-plugin@rival-mkt end -->\n";
    fs::write(&agents, rival).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(
        fs::read_to_string(&agents).unwrap(),
        format!("{rival}\n{}", instr_block()),
        "install must append our block, leaving the rival's untouched"
    );

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok, "uninstall failed: {out}");
    assert_eq!(fs::read_to_string(&agents).unwrap(), rival, "uninstall must strip only our block, keeping the rival's and the user's");
}

/// omp also reads the cross-tool `~/.agents/` directory, but that is shared with other
/// harnesses — never ours to write. Install must leave it untouched.
#[test]
fn omp_instructions_leaves_the_cross_tool_dir_unwritten() {
    let env = Env::new("instr-cross-tool");

    let (ok, out) = env.fixture(&["setup", "--agent", "omp"]);
    assert!(ok, "setup failed: {out}");
    assert!(env.base.join("AGENTS.md").exists(), "precondition: our block landed");

    let shared = env.root.join(".agents");
    assert!(!shared.exists(), "the cross-tool ~/.agents tree is never ours to create here: {}", shared.display());
}

/// Project scope has no instructions surface: omp's project context file is
/// `<repo>/.omp/AGENTS.md`, a file in the user's own repo, never one this backend
/// writes — so a `--project` install must leave both the repo and the user's agent dir
/// untouched, while still writing its project config.
#[test]
fn omp_instructions_user_scope_only() {
    let env = Env::new("instr-user-scope-only");
    let proj = env.root.join("proj");

    let (ok, out) = env.fixture(&["setup", "--agent", "omp", "--project", proj.to_str().unwrap()]);
    assert!(ok, "setup failed: {out}");
    assert!(proj.join(".omp").exists(), "precondition: the project install ran");
    assert!(!proj.join(".omp").join("AGENTS.md").exists(), "project scope must not write the repo's .omp/AGENTS.md");
    assert!(!env.base.join("AGENTS.md").exists(), "a project install must not touch the user's agent dir");
}
