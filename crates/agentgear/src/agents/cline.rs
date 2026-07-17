//! The cline backend: a full translate into cline's own file config. MCP goes
//! through the shared json renderer (Plain shape) into cline's global
//! `cline_mcp_settings.json` — a contested, mid-migration path, so we probe an
//! ordered candidate list (modern `~/.cline/data/settings/` first, legacy VS Code
//! globalStorage second) and pick the first that exists, else create the modern
//! one. The CLI's own `CLINE_MCP_SETTINGS_PATH`/`CLINE_DATA_DIR`/`CLINE_DIR`
//! overrides short-circuit that probe. Cline has no project-level MCP scope, so
//! MCP is always global.
//!
//! CC commands become cline **workflows** (markdown slash-commands) and CC hooks
//! become cline's file-based **hooks** (a script named exactly after the event).
//! These are scope-aware: user scope writes cline's global store under
//! `~/Documents/Cline/{Workflows,Hooks}`; project scope writes the repo's
//! `.clinerules/{workflows,hooks}`. Only `UserPromptSubmit`/`PreToolUse`/
//! `PostToolUse` have a 1:1 cline event; CC's `SessionStart` has no session-level
//! analog (cline hooks are task-level) and is skipped. Subagents have no cline file
//! surface and are skipped. See `docs/harness/cline.md` for the full mapping.
//!
//! Ownership: mcp servers are keyed by our server names; workflows are
//! plugin-prefixed files; hook scripts (whose filename cline forces to the bare
//! event name, so we cannot prefix them) carry an in-file ownership tag and we
//! never write or delete a hook file that isn't ours. So `remove` is exact and a
//! second reconcile is a true `NoOp`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use super::cchooks::hook_is_portable;
use super::confedit::{remove_file_idem, write_file_idem};
use super::mcpjson::{self, RemoteShape, ServerShape};
use super::report;
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, IoContext, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct ClineBackend;

/// cline's schema literal-matches the transport value: streamable HTTP is
/// `streamableHttp`, and the majority `"http"` value is refused — one bad entry
/// voids the whole `mcpServers` object, user servers included.
const SHAPE: ServerShape = ServerShape::plain().with_remote(RemoteShape::StreamableHttpValue);

impl AgentBackend for ClineBackend {
    fn id(&self) -> &'static str {
        "cline"
    }

    fn detect(&self) -> bool {
        // The config path is contested/mid-migration, so detection rides on any of
        // cline's candidate store roots (HOME-, XDG-, or override-based, so a test
        // redirecting those redirects detection too); `cline` on PATH is a bonus.
        which::which("cline").is_ok() || detect_dirs().iter().any(|d| d.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // `project` covers workflows/hooks (`.clinerules/`); MCP is global-only.
        Capabilities { plugins: false, mcp: true, hooks: true, scopes: &["user", "project"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Compose every surface (global mcp, scope-aware file hooks + workflows), so a
        // deleted hook script or workflow behind a healthy mcp file reads NeedsRepair.
        // MCP is global regardless of scope; hooks/workflows are scope-aware. `source`
        // is the one self_heal resolved for this agent (rehydrated `--path`, else the
        // compile-time default), so probe and reconcile render identical bytes.
        let comp = plugin.components(source)?;
        let mcp = mcpjson::probe_surface(&mcp_settings_path()?, &["mcpServers"], &comp.mcp_servers, SHAPE)?;
        let hooks = probe_hooks(&hooks_dir(scope)?, plugin.name, &comp.hooks)?;
        let commands = probe_workflows(&workflows_dir(scope)?, plugin.name, &comp.commands)?;
        Ok(report::compose([mcp, hooks, commands].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;

        let mut changed = false;
        let settings = mcp_settings_path()?;
        changed |= mcpjson::reconcile(&settings, &["mcpServers"], &comp.mcp_servers, SHAPE)? != Outcome::NoOp;
        changed |= reconcile_hooks(&hooks_dir(scope)?, plugin.name, &comp.hooks)?;

        let wf_root = workflows_dir(scope)?;
        for doc in &comp.commands {
            changed |= write_file_idem(&wf_root.join(workflow_file(plugin.name, doc)), workflow_body(doc).as_bytes())?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;

        let mut changed = false;
        let settings = mcp_settings_path()?;
        changed |= mcpjson::remove(&settings, &["mcpServers"], &comp.mcp_servers, SHAPE)? != Outcome::NoOp;
        changed |= remove_hooks(&hooks_dir(scope)?, plugin.name, &comp.hooks)?;

        // Workflows are plugin-prefixed files shared with the user's own workflows,
        // so we delete only ours by name (never a `remove_dir_all`).
        let wf_root = workflows_dir(scope)?;
        for doc in &comp.commands {
            changed |= remove_file_idem(&wf_root.join(workflow_file(plugin.name, doc)))?;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// A cline path override, honored only when non-empty: the CLI itself falls back to
/// its default on an empty value rather than resolving a relative path.
fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// cline's unified store root: `$CLINE_DIR`, else `~/.cline`.
fn store_root() -> Option<PathBuf> {
    env_path("CLINE_DIR").or_else(|| dirs::home_dir().map(|h| h.join(".cline")))
}

/// Store roots whose presence means "cline is configured on this machine".
fn detect_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // Detection resolves through the same chain the writes do, so a relocated store
    // counts exactly like the default one: `$CLINE_DIR` *is* `~/.cline` for a user
    // who set it, and `CLINE_DATA_DIR`/`CLINE_MCP_SETTINGS_PATH` can relocate the
    // settings file out from under both. Detecting on a narrower set than we write
    // to would skip a user whose only marker is the override they set.
    dirs.extend(store_root()); // modern unified store + CLI
    if let Ok(settings) = mcp_settings_path() {
        dirs.extend(settings.parent().map(Path::to_path_buf));
    }
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join("Documents").join("Cline")); // global rules/workflows/hooks
    }
    if let Some(config) = dirs::config_dir() {
        // legacy VS Code globalStorage (stable build); other VS Code variants exist
        // but this is the documented default.
        dirs.push(config.join("Code").join("User").join("globalStorage").join("saoudrizwan.claude-dev"));
    }
    dirs
}

/// The MCP settings file, global-only (cline has no project MCP scope). The CLI's
/// own overrides win outright, in its order: a full-path `CLINE_MCP_SETTINGS_PATH`,
/// then `CLINE_DATA_DIR`, then `CLINE_DIR`'s `data` subdir (all live-proven, see
/// `docs/research/verify-cline.md`). Those are CLI-only vocabulary, so an override
/// set means the CLI is the target and the legacy VS Code candidate is off the table.
///
/// Unoverridden, the path is mid-migration, so we probe: modern unified store first
/// (it wins merge conflicts), legacy VS Code globalStorage second, first existing
/// wins, else the modern path to create.
fn mcp_settings_path() -> Result<PathBuf> {
    if let Some(path) = env_path("CLINE_MCP_SETTINGS_PATH") {
        return Ok(path);
    }
    if let Some(data) = env_path("CLINE_DATA_DIR").or_else(|| env_path("CLINE_DIR").map(|d| d.join("data"))) {
        return Ok(data.join("settings").join("cline_mcp_settings.json"));
    }

    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".cline").join("data").join("settings").join("cline_mcp_settings.json"));
    }
    if let Some(config) = dirs::config_dir() {
        candidates.push(
            config
                .join("Code")
                .join("User")
                .join("globalStorage")
                .join("saoudrizwan.claude-dev")
                .join("settings")
                .join("cline_mcp_settings.json"),
        );
    }
    candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .or_else(|| candidates.into_iter().next())
        .ok_or_else(|| Error::Tree("no home or config directory; cannot locate cline_mcp_settings.json".into()))
}

/// Where hook scripts go for a scope: cline's global `~/Documents/Cline/Hooks`
/// (user) or the repo's `.clinerules/hooks` (project). User scope needs HOME.
///
/// `Hooks` is a sibling of `Rules`, not a child: cline's own resolver scans
/// `[~/Documents/Cline/Hooks, <CLINE_DIR|~/.cline>/hooks]` globally, so the
/// `Rules/Hooks` this once wrote was never read. Of the two, the `Documents` root
/// is the one no env var relocates, and it already holds our workflows.
fn hooks_dir(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => global_store().map(|s| s.join("Hooks")),
        Scope::Project { path } => Ok(path.join(".clinerules").join("hooks")),
    }
}

/// Where workflow (slash-command) files go: `~/Documents/Cline/Workflows` (user)
/// or `.clinerules/workflows` (project).
fn workflows_dir(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => global_store().map(|s| s.join("Workflows")),
        Scope::Project { path } => Ok(path.join(".clinerules").join("workflows")),
    }
}

fn global_store() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join("Documents").join("Cline"))
        .ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/Documents/Cline".into()))
}

// --- hooks -------------------------------------------------------------------

/// Map a CC hook event to cline's nearest shipped (v3.36) file-hook event. Only
/// three line up 1:1; cline's hooks are **task-level** (`TaskStart`/`TaskResume`/
/// `TaskCancel`), so CC's session-level `SessionStart`/`SessionEnd` have no analog —
/// mapping them to `TaskStart` would over-invoke (once per task, not once per
/// session). Unmapped events are skipped rather than written under a guessed name.
fn map_event(cc_event: &str) -> Option<&'static str> {
    match cc_event {
        "UserPromptSubmit" => Some("UserPromptSubmit"),
        "PreToolUse" => Some("PreToolUse"),
        "PostToolUse" => Some("PostToolUse"),
        _ => None,
    }
}

/// The in-file marker proving a hook script is ours. Cline forces a hook file's name
/// to the bare event, so we cannot namespace by filename; this tag lets `reconcile`
/// refuse to clobber a user's (or another plugin's) same-event hook and lets
/// `remove` delete only our own.
fn ownership_tag(plugin: &str) -> String {
    format!("agentgear-managed:{plugin}")
}

fn file_is_ours(path: &Path, plugin: &str) -> bool {
    fs::read_to_string(path).map(|s| s.contains(&ownership_tag(plugin))).unwrap_or(false)
}

/// A cline hook script: consume the event JSON on stdin, run each translated CC
/// command, and reply with `{"cancel","contextModification"}` (the CC command's
/// stdout becomes the injected context; we never cancel). Deterministic so a
/// re-reconcile is byte-identical. The JSON-string escape is best-effort via `sed`
/// (POSIX, present on the macOS/Linux platforms cline hooks support).
///
/// CC's per-tool `matcher` is dropped: cline's surface is one script per event with
/// no matcher field, so a `PreToolUse` hook scoped to `Bash` runs on every tool here.
/// Filtering inside the script would need cline's own tool vocabulary, which no doc
/// pins. See `docs/harness/cline.md` gotcha 9.
fn render_hook_script(plugin: &str, hooks: &[&HookBinding]) -> String {
    const TEMPLATE: &str = r##"#!/usr/bin/env bash
# __TAG__
# agentgear-translated cline hook. cline pipes the event JSON on stdin and reads
# {"cancel","contextModification"} from stdout; we inject the CC hook's stdout as
# context and never cancel. Do not edit: overwritten by the host's `setup`.
set -euo pipefail
cat >/dev/null
ctx=""
__CMDS__esc=$(printf '%s' "$ctx" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | sed ':a;N;$!ba;s/\n/\\n/g')
printf '{"cancel": false, "contextModification": "%s"}\n' "$esc"
"##;
    let mut cmds = String::new();
    for hook in hooks {
        // `$(...)` strips trailing newlines; `|| true` keeps a nonzero hook from
        // aborting the reply under `set -e`.
        let _ = writeln!(cmds, "ctx=\"$ctx$({} 2>/dev/null || true)\"", hook.command);
    }
    TEMPLATE.replace("__TAG__", &ownership_tag(plugin)).replace("__CMDS__", &cmds)
}

/// Write our hook script per mapped event (one file per event, all handlers for
/// that event folded into it). Never clobbers a hook file we don't own. Skips
/// entirely when nothing is writable so no empty dir/file is created for zero work.
fn reconcile_hooks(dir: &Path, plugin: &str, hooks: &[HookBinding]) -> Result<bool> {
    let mut by_event: BTreeMap<&'static str, Vec<&HookBinding>> = BTreeMap::new();
    for hook in hooks.iter().filter(|h| hook_is_portable(h)) {
        if let Some(event) = map_event(&hook.event) {
            by_event.entry(event).or_default().push(hook);
        }
    }
    let mut changed = false;
    for (event, group) in by_event {
        let path = dir.join(event);
        // Single-file-per-event surface: only (re)write an absent file or one we own.
        if path.exists() && !file_is_ours(&path, plugin) {
            continue;
        }
        if write_file_idem(&path, render_hook_script(plugin, &group).as_bytes())? {
            set_executable(&path)?;
            changed = true;
        }
    }
    Ok(changed)
}

/// The per-event `(path, rendered script)` files `reconcile_hooks` would write,
/// grouped exactly as it groups them (one file per event, all handlers folded in).
fn expected_hooks(dir: &Path, plugin: &str, hooks: &[HookBinding]) -> Vec<(PathBuf, Vec<u8>)> {
    let mut by_event: BTreeMap<&'static str, Vec<&HookBinding>> = BTreeMap::new();
    for hook in hooks.iter().filter(|h| hook_is_portable(h)) {
        if let Some(event) = map_event(&hook.event) {
            by_event.entry(event).or_default().push(hook);
        }
    }
    by_event.into_iter().map(|(event, group)| (dir.join(event), render_hook_script(plugin, &group).into_bytes())).collect()
}

/// Classify the file-per-event hook surface for `probe`. A same-named file WITHOUT
/// our ownership tag is the user's (reconcile never overwrites it), so it reads as
/// not-ours and contributes nothing — never our drift.
fn probe_hooks(dir: &Path, plugin: &str, hooks: &[HookBinding]) -> Result<Option<BackendState>> {
    let tag = ownership_tag(plugin);
    report::probe_files(&expected_hooks(dir, plugin, hooks), |_, existing| std::str::from_utf8(existing).is_ok_and(|s| s.contains(&tag)))
}

/// Classify the workflow (command) file surface for `probe`: plugin-prefixed files we
/// own by name, so a byte mismatch is our drift (`|_, _| true`).
fn probe_workflows(wf_root: &Path, plugin: &str, commands: &[MarkdownDoc]) -> Result<Option<BackendState>> {
    let expected: Vec<(PathBuf, Vec<u8>)> =
        commands.iter().map(|doc| (wf_root.join(workflow_file(plugin, doc)), workflow_body(doc).into_bytes())).collect();
    report::probe_files(&expected, |_, _| true)
}

/// Delete exactly our hook scripts (by ownership tag) for each mapped event; leave
/// a user's own same-event hook, and any unmapped event, untouched.
fn remove_hooks(dir: &Path, plugin: &str, hooks: &[HookBinding]) -> Result<bool> {
    let events: BTreeSet<&'static str> = hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event)).collect();
    let mut changed = false;
    for event in events {
        let path = dir.join(event);
        if path.exists() && file_is_ours(&path, plugin) {
            changed |= remove_file_idem(&path)?;
        }
    }
    Ok(changed)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path).io_ctx(|| format!("stat {}", path.display()))?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).io_ctx(|| format!("chmod +x {}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    // Cline hooks are macOS/Linux-only; on Windows the mode bit is meaningless.
    Ok(())
}

// --- workflows (commands) ----------------------------------------------------

/// `commands/hello.md` -> `<plugin>-hello.md`. Cline scans `Workflows/*.md` (flat),
/// so any nested path is flattened; the plugin prefix keeps the file identifiable
/// as ours for an exact `remove`.
fn workflow_file(plugin: &str, doc: &MarkdownDoc) -> String {
    format!("{plugin}-{}.md", flat_stem(&doc.rel, "commands/"))
}

fn flat_stem(rel: &str, prefix: &str) -> String {
    let stripped = rel.strip_prefix(prefix).unwrap_or(rel);
    let stem = stripped.strip_suffix(".md").unwrap_or(stripped);
    stem.replace(['/', '\\'], "-")
}

/// A cline workflow is plain markdown injected as the slash-command prompt: the CC
/// frontmatter (already split off by the IR) is dropped so it never leaks into the
/// prompt. Trailing newline for a stable, byte-identical re-reconcile.
fn workflow_body(doc: &MarkdownDoc) -> String {
    let mut body = doc.body.trim().to_string();
    body.push('\n');
    body
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &ClineBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "cline detected", status: CheckStatus::Ok("`cline` on PATH or a cline store dir present".into()) }
    } else {
        DoctorCheck {
            name: "cline detected",
            status: CheckStatus::Fail {
                problem: "cline not detected".into(),
                fix: "install the Cline VS Code extension or the `cline` CLI".into(),
            },
        }
    });

    let settings = match mcp_settings_path() {
        Ok(p) => p,
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp settings file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };

    let root = report::read_json_config(&mut checks, "mcp settings file", &settings);

    let Some(comp) = report::components(&mut checks, plugin, source) else {
        return checks;
    };

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcpServers"],
        "not in cline_mcp_settings.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    checks.push(check_workflows_present(&comp.commands, plugin.name));
    checks.push(check_hooks_present(&comp.hooks));

    checks
}

fn check_workflows_present(commands: &[MarkdownDoc], plugin: &str) -> DoctorCheck {
    let name = "translated workflows present";
    if commands.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no commands to translate".into()) };
    }
    let wf_root = match workflows_dir(&Scope::User) {
        Ok(p) => p,
        Err(e) => return DoctorCheck { name, status: CheckStatus::Warn(e.to_string()) },
    };
    let missing: Vec<String> = commands.iter().map(|d| workflow_file(plugin, d)).filter(|f| !wf_root.join(f).exists()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} workflow file(s) present", commands.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("workflow file(s) missing: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

fn check_hooks_present(hooks: &[HookBinding]) -> DoctorCheck {
    let name = "translated hooks present";
    let events: BTreeSet<&'static str> = hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event)).collect();
    if events.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no hooks map to a cline event".into()) };
    }
    let hook_root = match hooks_dir(&Scope::User) {
        Ok(p) => p,
        Err(e) => return DoctorCheck { name, status: CheckStatus::Warn(e.to_string()) },
    };
    let missing: Vec<&str> = events.iter().copied().filter(|e| !hook_root.join(e).exists()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} hook script(s) present", events.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("hook script(s) missing: {}", missing.join(", ")),
                fix: "run the host's `setup` (or a user hook already owns that event)".into(),
            },
        }
    }
}
