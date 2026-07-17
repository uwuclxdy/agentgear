//! Hermetic cline-backend lifecycle, fully isolated from the real cline stores.
//! No docker, no auth, no `cline` binary: the backend only ever writes cline's
//! config files, so we drive `host_fixture setup --agent cline` against a temp
//! `HOME` (+ XDG dirs) and assert the written `cline_mcp_settings.json`, the
//! translated workflow markdown, and the file-based hook script by reading them
//! back. `detect()` passes off the pre-created `~/.cline` store dir alone.
//!
//! Every path the backend touches derives from `HOME`/`XDG_CONFIG_HOME`, which we
//! point at a throwaway temp root — so proving our files land under that root (and
//! the seeded user entries survive) also proves the backend never reaches the
//! developer's real cline config.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_host_fixture");

/// A foreign mcp server + an unrelated top-level key that MUST outlive our install
/// and uninstall untouched.
const SEED_SETTINGS: &str = r#"{
  "someUserSetting": true,
  "mcpServers": {
    "theirs": { "command": "their-server", "args": [] }
  }
}
"#;

/// A user-owned hook on a cline event we never map to (`TaskStart`). It carries no
/// agentgear ownership tag, so the backend must leave it fully untouched.
const SEED_FOREIGN_HOOK: &str = "#!/usr/bin/env bash\n# user's own TaskStart hook\necho '{\"cancel\": false}'\n";

struct Env {
    root: PathBuf,
    /// Modern unified store settings file (`~/.cline/data/settings/...`) — the path
    /// the backend picks when it already exists (it wins the migration merge).
    settings: PathBuf,
    workflow: PathBuf,
    /// `~/Documents/Cline/Hooks/<Event>`: one of the two global dirs cline's own
    /// `resolveHooksConfigSearchPaths` scans (`docs/research/verify-cline.md` #3).
    /// A `Rules/` segment here would be silently inert.
    our_hook: PathBuf,
    foreign_hook: PathBuf,
    config: PathBuf,
    data: PathBuf,
    run: PathBuf,
    /// PATH holding only the fixture binary's dir, so `which("cline")` (and every
    /// other backend's PATH probe) stays false and detection rides on `~/.cline`.
    path: OsString,
    /// The CLI's own config-dir overrides. Unset by default; the override test sets
    /// them to prove the backend resolves the settings file the way the CLI does.
    overrides: Vec<(&'static str, PathBuf)>,
}

impl Env {
    /// `name` must be unique per test: `std::process::id()` alone is constant across
    /// every test in this binary, so two tests sharing one root race on it under
    /// cargo's default parallel test threads (one's `remove_dir_all` can wipe the
    /// other's mid-flight fixture invocation). Mirrors `tests/e2e.rs`'s pattern.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ez-cline-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let global = root.join("Documents").join("Cline");
        let env = Env {
            settings: root.join(".cline").join("data").join("settings").join("cline_mcp_settings.json"),
            workflow: global.join("Workflows").join("ez-fixture-plugin-hello.md"),
            our_hook: global.join("Hooks").join("UserPromptSubmit"),
            foreign_hook: global.join("Hooks").join("TaskStart"),
            config: root.join("config"),
            data: root.join("data"),
            run: root.join("run"),
            path: fixture_dir(),
            overrides: Vec::new(),
            root,
        };
        // Pre-create `~/.cline/data/settings` so detect() passes with no `cline` on
        // PATH, and seed an unrelated user config the lifecycle must preserve.
        fs::create_dir_all(env.settings.parent().unwrap()).unwrap();
        fs::write(&env.settings, SEED_SETTINGS).unwrap();
        // Seed a user-owned hook on an event we don't touch; it must survive verbatim.
        fs::create_dir_all(env.foreign_hook.parent().unwrap()).unwrap();
        fs::write(&env.foreign_hook, SEED_FOREIGN_HOOK).unwrap();
        for dir in [&env.data, &env.run] {
            fs::create_dir_all(dir).unwrap();
        }
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("PATH", &self.path);
        // A stray override on the dev box would relocate every other test's settings
        // file, so clear all three and set back only what this Env asked for.
        for var in ["CLINE_DIR", "CLINE_DATA_DIR", "CLINE_MCP_SETTINGS_PATH"] {
            cmd.env_remove(var);
        }
        for (var, value) in &self.overrides {
            cmd.env(var, value);
        }
    }

    fn fixture(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        self.apply(&mut cmd);
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn settings(&self) -> String {
        fs::read_to_string(&self.settings).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// PATH with only the fixture binary's directory: no `cline`, no sibling agent
/// CLIs, so the fan-out stays a pure cline exercise regardless of the dev box.
fn fixture_dir() -> OsString {
    Path::new(BIN).parent().map(|d| d.as_os_str().to_os_string()).unwrap_or_default()
}

#[test]
fn cline_full_lifecycle() {
    let env = Env::new("full-lifecycle");

    // install: translates mcp + hooks + commands into cline's file config.
    let (ok, out) = env.fixture(&["setup", "--agent", "cline"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    // mcp: our server landed under `mcpServers`, Plain shape; seed survived.
    let s = env.settings();
    assert!(s.contains("ez-fixture"), "our mcp server key missing:\n{s}");
    assert!(s.contains("host_fixture"), "our mcp command missing:\n{s}");
    assert!(s.contains("theirs") && s.contains("their-server"), "seeded mcp server was clobbered:\n{s}");
    assert!(s.contains("someUserSetting"), "seeded top-level key was clobbered:\n{s}");
    // Plain shape emits no `disabled`/`autoApprove` defaults.
    assert!(!s.contains("autoApprove"), "Plain shape should not emit autoApprove:\n{s}");

    // remote mcp: cline's schema literal-matches the transport value — its
    // streamable-HTTP discriminator is `streamableHttp`, and a `type:"http"` entry
    // voids the whole mcpServers object (user servers included).
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-http"],
        serde_json::json!({"type": "streamableHttp", "url": "http://127.0.0.1:39621/mcp", "headers": {}}),
        "http remote arm mismatch:\n{s}"
    );
    assert_eq!(
        parsed["mcpServers"]["ez-fixture-sse"],
        serde_json::json!({"type": "sse", "url": "http://127.0.0.1:39622/sse", "headers": {}}),
        "sse remote arm mismatch:\n{s}"
    );

    // workflows: one plugin-prefixed markdown per command, body only (no frontmatter).
    assert!(env.workflow.exists(), "workflow md not written: {}", env.workflow.display());
    let wf = fs::read_to_string(&env.workflow).unwrap();
    assert!(wf.contains("Say hello"), "command body not translated to the workflow:\n{wf}");
    assert!(!wf.contains("description:"), "frontmatter leaked into the workflow:\n{wf}");

    // hooks: UserPromptSubmit script landed, ours, invoking the CC command.
    assert!(env.our_hook.exists(), "UserPromptSubmit hook not written: {}", env.our_hook.display());
    let hk = fs::read_to_string(&env.our_hook).unwrap();
    assert!(hk.contains("agentgear-managed:ez-fixture-plugin"), "hook missing ownership tag:\n{hk}");
    assert!(hk.contains("host_fixture check-restart"), "hook does not invoke the CC command:\n{hk}");
    assert!(hk.contains("contextModification"), "hook missing the cline reply shape:\n{hk}");
    // SessionStart has no cline analog: it must be skipped, not guessed onto an event.
    let hooks_dir = env.our_hook.parent().unwrap();
    assert!(!hooks_dir.join("SessionStart").exists(), "SessionStart was written despite having no cline analog");
    // `Rules/Hooks` is a dir cline never scans: a hook written there is inert.
    let unscanned = env.root.join("Documents").join("Cline").join("Rules").join("Hooks");
    assert!(!unscanned.exists(), "hook written to a dir cline never scans: {}", unscanned.display());
    // the user's own TaskStart hook is untouched.
    assert_eq!(fs::read_to_string(&env.foreign_hook).unwrap(), SEED_FOREIGN_HOOK, "user's TaskStart hook was clobbered");

    // executable bit set on unix (cline requires it).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&env.our_hook).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "hook script is not executable: mode {mode:o}");
    }

    // safety: everything we wrote is under the throwaway temp root.
    for p in [&env.settings, &env.workflow, &env.our_hook] {
        assert!(p.starts_with(&env.root), "backend wrote outside the temp root: {}", p.display());
    }

    // idempotent: a second identical reconcile is a true NoOp (no write).
    let (ok, out) = env.fixture(&["setup", "--agent", "cline"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    // uninstall: our entries/files gone, the user's kept.
    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let s = env.settings();
    assert!(!s.contains("ez-fixture"), "our mcp server survived uninstall:\n{s}");
    assert!(s.contains("theirs") && s.contains("their-server"), "uninstall removed the seeded mcp server:\n{s}");
    assert!(s.contains("someUserSetting"), "uninstall removed the seeded top-level key:\n{s}");
    assert!(!env.workflow.exists(), "our workflow survived uninstall: {}", env.workflow.display());
    assert!(!env.our_hook.exists(), "our hook survived uninstall: {}", env.our_hook.display());
    assert_eq!(fs::read_to_string(&env.foreign_hook).unwrap(), SEED_FOREIGN_HOOK, "uninstall touched the user's TaskStart hook");

    // the post-uninstall config still parses: a clean re-install lands again.
    let (ok, out) = env.fixture(&["setup", "--agent", "cline"]);
    assert!(ok && out == "Installed", "re-install after uninstall should install, got {out}");
    assert!(env.settings().contains("ez-fixture"), "re-install did not re-add our server");
}

/// The CLI resolves its mcp settings file through three overrides before falling
/// back to `~/.cline`, and an empty value falls back rather than resolving to a
/// relative path. Each level is live-proven against `cline@3.0.40`
/// (`docs/research/verify-cline.md`); a settings file written to the HOME default
/// while any of them is set is a file the CLI never reads.
#[test]
fn cline_honors_the_cli_config_dir_overrides() {
    let mut env = Env::new("env-overrides");
    let default_settings = env.settings.clone();

    // `CLINE_DIR` relocates the whole store: `<dir>/data/settings/…`.
    let store = env.root.join("relocated-store");
    env.overrides = vec![("CLINE_DIR", store.clone())];
    let (ok, out) = env.fixture(&["setup", "--agent", "cline"]);
    assert!(ok, "setup under CLINE_DIR failed: {out}");
    let via_dir = store.join("data").join("settings").join("cline_mcp_settings.json");
    assert!(via_dir.exists(), "CLINE_DIR ignored; nothing at {}", via_dir.display());
    assert!(fs::read_to_string(&via_dir).unwrap().contains("ez-fixture"), "our server missing under CLINE_DIR");

    // `CLINE_DATA_DIR` beats `CLINE_DIR` and skips the `data` segment.
    let data = env.root.join("relocated-data");
    env.overrides = vec![("CLINE_DIR", store.clone()), ("CLINE_DATA_DIR", data.clone())];
    let (ok, out) = env.fixture(&["setup", "--agent", "cline"]);
    assert!(ok, "setup under CLINE_DATA_DIR failed: {out}");
    let via_data = data.join("settings").join("cline_mcp_settings.json");
    assert!(via_data.exists(), "CLINE_DATA_DIR ignored; nothing at {}", via_data.display());

    // `CLINE_MCP_SETTINGS_PATH` is a full path and beats both dir-level overrides.
    let full = env.root.join("elsewhere").join("custom-settings.json");
    env.overrides = vec![("CLINE_DIR", store), ("CLINE_DATA_DIR", data), ("CLINE_MCP_SETTINGS_PATH", full.clone())];
    let (ok, out) = env.fixture(&["setup", "--agent", "cline"]);
    assert!(ok, "setup under CLINE_MCP_SETTINGS_PATH failed: {out}");
    assert!(full.exists(), "CLINE_MCP_SETTINGS_PATH ignored; nothing at {}", full.display());
    assert!(fs::read_to_string(&full).unwrap().contains("ez-fixture"), "our server missing at the overridden path");

    // The HOME default keeps only its seed: no override run may write there.
    let seeded = fs::read_to_string(&default_settings).unwrap();
    assert!(!seeded.contains("ez-fixture"), "an override run wrote to the HOME default the CLI never reads:\n{seeded}");
}

/// Detection resolves through the same env chain as the writes. A user whose store
/// lives entirely behind `CLINE_DATA_DIR` has no `~/.cline` and no `~/Documents/Cline`
/// to detect on, so a HOME-only `detect()` would skip the backend outright and write
/// nothing, while `reconcile` would happily have written to the override.
#[test]
fn cline_detects_a_store_that_exists_only_behind_an_override() {
    let mut env = Env::new("detect-via-override");
    // Strip every HOME-based marker: the override is the only evidence cline exists.
    fs::remove_dir_all(env.root.join(".cline")).unwrap();
    fs::remove_dir_all(env.root.join("Documents")).unwrap();

    let data = env.root.join("only-marker");
    fs::create_dir_all(data.join("settings")).unwrap();
    env.overrides = vec![("CLINE_DATA_DIR", data.clone())];

    let (ok, out) = env.fixture(&["setup", "--agent", "cline"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "an undetected backend is silently skipped; got {out}");
    let settings = data.join("settings").join("cline_mcp_settings.json");
    assert!(settings.exists(), "nothing written to the override-only store: {}", settings.display());
}

/// An empty value falls back to the next level, matching the CLI (live-proven: all
/// three set to `""` still resolve `~/.cline/data/settings/…`). Without that filter
/// an empty `CLINE_DIR` would resolve `data/settings/cline_mcp_settings.json`
/// relative to the process CWD, writing a stray config into whatever dir the host
/// binary happened to run from.
#[test]
fn cline_treats_an_empty_override_as_unset() {
    let mut env = Env::new("empty-overrides");
    env.overrides = vec![("CLINE_DIR", PathBuf::new()), ("CLINE_DATA_DIR", PathBuf::new()), ("CLINE_MCP_SETTINGS_PATH", PathBuf::new())];

    let (ok, out) = env.fixture(&["setup", "--agent", "cline"]);
    assert!(ok, "setup with empty overrides failed: {out}");
    assert_eq!(out, "Installed", "empty overrides should fall back and install, got {out}");
    assert!(env.settings().contains("ez-fixture"), "empty overrides did not fall back to the HOME default:\n{}", env.settings());
}

/// Cline forces a hook script's filename to the bare event name (no plugin
/// namespacing), so `UserPromptSubmit` is the one path both a user's own hook and
/// ours would collide on. This is the load-bearing case for the ownership-tag
/// mechanism: seed a foreign, untagged script at that exact path *before* install
/// and prove the backend never overwrites or deletes it (`cline_full_lifecycle`
/// above only seeds a foreign hook on `TaskStart`, an event we never map, which
/// would pass even with the tag check deleted).
#[test]
fn cline_never_touches_foreign_hook_on_mapped_event() {
    let env = Env::new("foreign-mapped-hook");
    let foreign = "#!/usr/bin/env bash\n# user's own UserPromptSubmit hook, no agentgear tag\necho '{\"cancel\": false}'\n";
    fs::create_dir_all(env.our_hook.parent().unwrap()).unwrap();
    fs::write(&env.our_hook, foreign).unwrap();

    let (ok, out) = env.fixture(&["setup", "--agent", "cline"]);
    assert!(ok, "setup failed: {out}");

    // The foreign UserPromptSubmit script must survive byte-for-byte: no tag added,
    // no CC command spliced in.
    let after_install = fs::read_to_string(&env.our_hook).unwrap();
    assert_eq!(after_install, foreign, "foreign UserPromptSubmit hook was overwritten on install");

    // Other surfaces still land normally; only the hook write was skipped.
    assert!(env.workflow.exists(), "workflow should still install despite the foreign hook collision");
    assert!(env.settings().contains("ez-fixture"), "mcp server should still install despite the foreign hook collision");

    let (ok, out) = env.fixture(&["uninstall"]);
    assert!(ok && !out.is_empty(), "uninstall failed: {out}");

    // Never delete a hook file we don't own.
    let after_uninstall = fs::read_to_string(&env.our_hook).unwrap();
    assert_eq!(after_uninstall, foreign, "foreign UserPromptSubmit hook was deleted on uninstall");
}
