//! The crush (charmbracelet) backend: a full translate into crush's own config,
//! `~/.config/crush/crush.json` (override `CRUSH_GLOBAL_CONFIG`). MCP and hooks
//! share that one file — the root `mcp` map and the root `hooks` key — so a
//! reconcile is a single `json_edit` covering both. MCP reuses the shared json
//! renderer's `Typed` shape (`{type:"stdio",command,args,env}`; crush requires an
//! explicit per-server `type`, enum `stdio|http|sse`); hooks are flat
//! `{command,matcher?}` entries under `hooks.PreToolUse`. Every mcp key is our own
//! server name and every hook is matched by its command string, so `remove` is
//! exact and a second reconcile is a true `NoOp`.
//!
//! - **commands**: real, file-writable, TUI-only surface. crush directory-loads
//!   `.md` files recursively (`filepath.WalkDir` in `internal/commands/commands.go`) under
//!   `~/.config/crush/commands` (user) / `<project>/.crush/commands` (project — its
//!   default `DataDirectory`), so a plugin-named `<plugin>/` subdir namespaces our
//!   commands (crush's own id becomes `user:<plugin>:<name>`) without colliding with
//!   the user's own; `remove` drops the whole subtree. crush also reads a second user
//!   dir, `~/.crush/commands`, unwritten here (same as the skills surface's unwritten
//!   dirs). The loader does **not** strip YAML frontmatter, so we write the parsed
//!   `body` only — a verbatim copy would leak CC frontmatter into the prompt text.
//!
//! Skipped surfaces (see `docs/harness/crush.md`):
//! - **agents**: no file-writable subagent surface (issue #1807 open), so `agents`
//!   is still dropped, not guessed.
//! - **skills**: bare `~/.config/crush/skills/<name>/SKILL.md` (user) /
//!   `<project>/.crush/skills/<name>/SKILL.md` (project), tagged for ownership. crush
//!   requires `name`+`description`, which the shared renderer ensures.
//! - **hook events other than `PreToolUse`**: crush defines only that one event
//!   today; every other CC event has no analog and is skipped.
//!
//! Unlike codex, crush hooks are NOT trust-gated — a written hook fires
//! automatically, so a translated hook is live the moment crush next reads the file.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::cchooks::hook_is_portable;
use super::confedit::{json_edit, json_obj_at, write_file_idem};
use super::mcpjson::{self, ServerShape};
use super::report;
use super::skillsdir;
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, MarkdownDoc, McpServer};
// The unit test builds server fixtures with `super::McpKind`; production no longer
// references it directly (the mcp checks moved to `report`), so the re-export is
// test-only to avoid an unused-import warning.
#[cfg(test)]
use crate::components::McpKind;
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, IoContext, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct CrushBackend;

impl AgentBackend for CrushBackend {
    fn id(&self) -> &'static str {
        "crush"
    }

    fn detect(&self) -> bool {
        // `~/.config/crush` is XDG-based with a documented `CRUSH_GLOBAL_CONFIG`
        // override, so a test redirecting either points detection at the same temp
        // dir; the `crush` CLI on PATH is a bonus, its absence never implies absent.
        which::which("crush").is_ok() || crush_config_dir_opt().is_some_and(|d| d.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // mcp + hooks (PreToolUse) + commands + skills translate; agents have no
        // file-writable subagent surface (issue #1807).
        Capabilities { plugins: false, mcp: true, hooks: true, commands: true, agents: false, skills: true, scopes: &["user", "project"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Compose every surface sharing crush.json (mcp + PreToolUse hooks) plus the
        // skills and commands dirs: a dropped entry behind an otherwise-healthy
        // surface now reads NeedsRepair instead of Healthy. `source` is the one
        // self_heal resolved for this agent (rehydrated `--path`, else the
        // compile-time default), so probe and reconcile render identical bytes.
        let comp = plugin.components(source)?;
        let config = config_file(scope)?;
        let mcp = mcpjson::probe_surface(&config, &["mcp"], &comp.mcp_servers, ServerShape::typed())?;
        let hooks = report::probe_json_entries(&config, &hook_entries(&comp.hooks))?;
        let skills = skillsdir::probe(&skills_root(scope)?, plugin, &comp.skills)?;
        let cmd_root = commands_root(scope)?.join(plugin.name);
        let commands = report::probe_files(&expected_commands(&cmd_root, &comp.commands), |_, _| true)?;
        Ok(report::compose([mcp, hooks, skills, commands].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let mut changed = reconcile_config(&config_file(scope)?, &comp.mcp_servers, &comp.hooks)?;
        changed |= skillsdir::reconcile(&skills_root(scope)?, plugin, &comp.skills)?;
        let cmd_root = commands_root(scope)?.join(plugin.name);
        for doc in &comp.commands {
            changed |= write_file_idem(&cmd_root.join(command_rel(doc)), render_command(doc).as_bytes())?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let mut changed = remove_config(&config_file(scope)?, &portable_names(&comp.mcp_servers), &comp.hooks)?;
        changed |= skillsdir::remove(&skills_root(scope)?, plugin, &comp.skills)?;
        // We own the whole `commands/<plugin>/` subtree (crush walks it recursively),
        // so a recursive drop is exact and never reaches the user's own commands.
        let cmd_root = commands_root(scope)?.join(plugin.name);
        if cmd_root.exists() {
            fs::remove_dir_all(&cmd_root).io_ctx(|| format!("removing {}", cmd_root.display()))?;
            changed = true;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The global crush config dir, honoring `CRUSH_GLOBAL_CONFIG` (its documented
/// override) then `$XDG_CONFIG_HOME/crush`. `_opt` never errors so `detect` can
/// call it; the env/XDG fallbacks mean a test redirecting either redirects both
/// detection and the write target.
fn crush_config_dir_opt() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CRUSH_GLOBAL_CONFIG").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    // Local (not Roaming) AppData on Windows — crush reads `%LOCALAPPDATA%\crush`;
    // on Linux/macOS this is identical to `config_dir` (`$XDG_CONFIG_HOME`).
    dirs::config_local_dir().map(|c| c.join("crush"))
}

fn crush_config_dir() -> Result<PathBuf> {
    crush_config_dir_opt().ok_or_else(|| {
        Error::Tree("no config directory (HOME/XDG_CONFIG_HOME/CRUSH_GLOBAL_CONFIG unset); cannot locate ~/.config/crush".into())
    })
}

/// The single `crush.json` we read-modify-write for a scope: the global
/// `~/.config/crush/crush.json` (user) or `<cwd>/crush.json` (project — crush lets
/// a project config override the global, per the brief).
fn config_file(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(crush_config_dir()?.join("crush.json")),
        Scope::Project { path } => Ok(path.join("crush.json")),
    }
}

/// The skills root for a scope: the global `~/.config/crush/skills` (a crush-managed
/// skill dir) or `<project>/.crush/skills`. crush directory-loads a bare
/// `<name>/SKILL.md` under either. `$CRUSH_SKILLS_DIR`, if set, makes crush read ONLY
/// that dir — an accepted v1 edge we don't consult here.
fn skills_root(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(crush_config_dir()?.join("skills")),
        Scope::Project { path } => Ok(path.join(".crush").join("skills")),
    }
}

/// The commands root for a scope: `~/.config/crush/commands` (user — the first of
/// crush's two user command dirs, `~/.crush/commands` stays unwritten) or
/// `<project>/.crush/commands` (project — crush's default `DataDirectory`). crush
/// directory-loads `.md` files recursively (`filepath.WalkDir`), so a plugin-named
/// subdir under either root namespaces our commands without a per-file rename.
fn commands_root(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(crush_config_dir()?.join("commands")),
        Scope::Project { path } => Ok(path.join(".crush").join("commands")),
    }
}

/// Server names `reconcile` actually writes (non-portable ones are skipped).
/// `remove` keys off the same set so it never deletes a user server that happens
/// to share a name with one we declared but never wrote.
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
}

// --- hooks -------------------------------------------------------------------

/// Crush defines exactly one hook event today — `PreToolUse` — matched
/// case-insensitively but written in its canonical casing. Every other CC event
/// has no crush analog and is skipped rather than written under a guessed name.
fn map_event(cc_event: &str) -> Option<&'static str> {
    cc_event.eq_ignore_ascii_case("PreToolUse").then_some("PreToolUse")
}

/// The `(array key_path, rendered entry)` pairs `probe` checks are present under
/// `hooks.<event>`, mirroring `reconcile_config`'s writable-hook filter exactly
/// (portable AND a mapped crush event) so probe and reconcile can never disagree.
fn hook_entries(hooks: &[HookBinding]) -> Vec<(Vec<String>, Value)> {
    hooks
        .iter()
        .filter(|h| hook_is_portable(h))
        .filter_map(|h| map_event(&h.event).map(|event| (vec!["hooks".to_string(), event.to_string()], render_hook_entry(h))))
        .collect()
}

/// A crush hook entry is a flat `{command, matcher?}` object directly in the event
/// array (no CC-style nested `hooks` list); the optional `name`/`timeout` fields are
/// omitted (crush defaults `timeout` to 30 and `name` is a cosmetic TUI label).
fn render_hook_entry(hook: &HookBinding) -> Value {
    let mut obj = Map::new();
    obj.insert("command".into(), Value::from(hook.command.clone()));
    if let Some(matcher) = &hook.matcher {
        obj.insert("matcher".into(), Value::from(matcher.clone()));
    }
    Value::Object(obj)
}

// --- reconcile / remove (one file, one edit) ---------------------------------

/// Insert/update our mcp servers under the root `mcp` map and add-if-absent our
/// `PreToolUse` hook entries under `hooks.PreToolUse`, in a single `json_edit` (mcp
/// and hooks live in the same `crush.json`). Non-portable servers/hooks and events
/// with no crush analog are skipped; when nothing survives, `json_edit` is not
/// entered so no empty `mcp`/`hooks` key is created for a plugin with nothing to
/// translate. Idempotent: a hook already present (deep-equal) is not re-added, and
/// an unchanged document skips the write -> a true `NoOp`.
fn reconcile_config(config: &Path, servers: &[McpServer], hooks: &[HookBinding]) -> Result<bool> {
    let portable: Vec<&McpServer> = servers.iter().filter(|s| s.is_portable()).collect();
    let writable_hooks: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|e| (e, h))).collect();
    if portable.is_empty() && writable_hooks.is_empty() {
        return Ok(false);
    }
    json_edit(config, |root| {
        if !portable.is_empty() {
            let mcp = json_obj_at(root, &["mcp"]);
            for server in &portable {
                if let Some(body) = mcpjson::render_server(server, ServerShape::typed()) {
                    mcp.insert(server.name.clone(), body);
                }
            }
        }
        if !writable_hooks.is_empty() {
            let events = json_obj_at(root, &["hooks"]);
            for (event, hook) in &writable_hooks {
                let entry = render_hook_entry(hook);
                let list = events.entry((*event).to_string()).or_insert_with(|| Value::Array(Vec::new()));
                if let Value::Array(arr) = list
                    && !arr.iter().any(|e| e == &entry)
                {
                    arr.push(entry);
                }
            }
        }
        Ok(())
    })
}

/// Strip exactly our mcp server keys and our hook entries (matched by command
/// string) from the one `crush.json`, dropping a hook event array we emptied. A
/// user server or hook sharing a key/event with ours survives; the file itself is
/// left in place (merge-safe).
fn remove_config(config: &Path, server_names: &[&str], hooks: &[HookBinding]) -> Result<bool> {
    if !config.exists() {
        return Ok(false);
    }
    // Match reconcile's writable-hook filter exactly (portable AND a mapped crush
    // event): a portable hook under an unmapped CC event was never written, so its
    // command must never be a removal candidate — and we only ever touch the crush
    // events we manage, never a user's own event array.
    let ours: BTreeSet<&str> =
        hooks.iter().filter(|h| hook_is_portable(h) && map_event(&h.event).is_some()).map(|h| h.command.as_str()).collect();
    let managed: BTreeSet<&str> = hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event)).collect();
    json_edit(config, |root| {
        if let Some(mcp) = root.get_mut("mcp").and_then(Value::as_object_mut) {
            for name in server_names {
                mcp.remove(*name);
            }
        }
        if let Some(events) = root.get_mut("hooks").and_then(Value::as_object_mut) {
            // Only clean up an event array we actually emptied — a user's pre-existing
            // empty array under an event we manage (or any unmanaged event) survives.
            let mut emptied: Vec<String> = Vec::new();
            for event in &managed {
                if let Some(arr) = events.get_mut(*event).and_then(Value::as_array_mut) {
                    let before = arr.len();
                    arr.retain(|e| e.get("command").and_then(Value::as_str).is_none_or(|c| !ours.contains(c)));
                    if arr.len() < before && arr.is_empty() {
                        emptied.push((*event).to_string());
                    }
                }
            }
            for event in emptied {
                events.remove(&event);
            }
        }
        Ok(())
    })
}

// --- commands ------------------------------------------------------------------

/// `commands/hello.md` -> `hello.md` (extension kept — crush wants raw markdown,
/// not TOML), preserving any subdir so a nested CC command keeps its own nesting
/// under our plugin subdir.
fn command_rel(doc: &MarkdownDoc) -> String {
    doc.rel.strip_prefix("commands/").unwrap_or(&doc.rel).to_string()
}

/// Render a CC command doc as a crush command file: body only. crush's loader
/// (`internal/commands/commands.go`) does not split frontmatter — the whole file
/// becomes the literal prompt text — so copying `doc.raw` verbatim would leak the
/// CC `---` frontmatter block into it; `doc.body` already excludes it (the
/// frontmatter/body split happens at parse time). Deterministic so a re-reconcile
/// is byte-identical.
fn render_command(doc: &MarkdownDoc) -> String {
    let mut out = doc.body.trim().to_string();
    out.push('\n');
    out
}

/// The `(path, rendered bytes)` command files `probe` compares against disk, keyed
/// off the same `command_rel` + `render_command` `reconcile` writes.
fn expected_commands(cmd_root: &Path, commands: &[MarkdownDoc]) -> Vec<(PathBuf, Vec<u8>)> {
    commands.iter().map(|doc| (cmd_root.join(command_rel(doc)), render_command(doc).into_bytes())).collect()
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &CrushBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "crush detected", status: CheckStatus::Ok("`crush` on PATH or ~/.config/crush present".into()) }
    } else {
        DoctorCheck {
            name: "crush detected",
            status: CheckStatus::Fail {
                problem: "crush CLI not detected".into(),
                fix: "install it with `npm install -g @charmland/crush`".into(),
            },
        }
    });

    let base = match crush_config_dir() {
        Ok(base) => base,
        Err(e) => {
            checks.push(DoctorCheck { name: "config file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };
    let config = base.join("crush.json");

    let root = report::read_json_config(&mut checks, "config file", &config);

    let Some(comp) = report::components(&mut checks, plugin, source) else {
        return checks;
    };

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcp"],
        "not under `mcp` in crush.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    checks.push(check_hooks_present(&comp.hooks, root.as_ref()));
    checks.push(check_commands_present(&comp.commands, &base.join("commands").join(plugin.name)));

    checks
}

/// Only `PreToolUse` hooks translate; a plugin whose hooks are all other events (or
/// all non-portable) has nothing to check. A present hook is a plain `Ok` — crush
/// hooks are not trust-gated, so they fire the moment crush reads the file.
fn check_hooks_present(hooks: &[HookBinding], root: Option<&Value>) -> DoctorCheck {
    let name = "translated hooks present";
    let ours: Vec<&str> =
        hooks.iter().filter(|h| hook_is_portable(h) && map_event(&h.event).is_some()).map(|h| h.command.as_str()).collect();
    if ours.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no PreToolUse hooks to translate".into()) };
    }
    let commands: BTreeSet<&str> = root
        .and_then(|r| r.get("hooks"))
        .and_then(|h| h.get("PreToolUse"))
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|e| e.get("command").and_then(Value::as_str)).collect())
        .unwrap_or_default();
    let missing: Vec<&str> = ours.iter().copied().filter(|c| !commands.contains(c)).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} PreToolUse hook(s) present", ours.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("PreToolUse hook(s) missing from crush.json: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

fn check_commands_present(commands: &[MarkdownDoc], cmd_root: &Path) -> DoctorCheck {
    let name = "translated commands present";
    if commands.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no commands to translate".into()) };
    }
    let missing: Vec<String> = commands.iter().map(command_rel).filter(|rel| !cmd_root.join(rel).exists()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} command file(s) present", commands.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("command file(s) missing: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/crush.rs"]
mod crush_tests;
