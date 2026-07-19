//! The goose backend: a partial translate into goose's own config. MCP servers
//! become entries in the `extensions:` map of `~/.config/goose/config.yaml` (YAML,
//! keyed by extension name, with goose's own field names — `cmd`/`args`/`envs`,
//! not CC's `command`/`env`). Hooks land in a plugin-owned dir under the cross-tool
//! "Open Plugins" spec path `~/.agents/plugins/<plugin>/hooks/hooks.json`, in CC's
//! exact event shape (goose names its events after CC's as a superset). Every
//! extension key is our own server name and the whole plugin hooks dir is ours, so
//! `remove` is exact and a second reconcile is a true `NoOp`.
//!
//! Two load-bearing goose caveats (see `docs/harness/goose.md`):
//! - the YAML editor is NOT comment-preserving (no pure-Rust comment-preserving
//!   YAML writer exists); a write that changes the config re-renders it and drops
//!   comments. Reconcile stays semantically no-op-aware so an already-converged
//!   config is never rewritten, keeping a user's comments intact in steady state.
//! - MCP registration is user-scope-only: goose has a single user `config.yaml`
//!   with no project-level extensions file, so `probe`/`remove` anchor on it.
//! - `GOOSE_PATH_ROOT` wins unconditionally over `XDG_CONFIG_HOME`/`HOME` (goose's
//!   own precedence) and relocates both surfaces: `<root>/config/config.yaml` and
//!   `<root>/.agents/plugins`, not the ordinary XDG/HOME-derived paths.
//!
//! Skills land inside the plugin dir goose already owns
//! (`~/.agents/plugins/<plugin>/skills/<name>/SKILL.md`), tagged for ownership;
//! `remove` drops the whole plugin dir so they need no separate removal.
//!
//! Skipped surfaces (see `docs/harness/goose.md`): commands + agents (both would
//! translate into goose's single `recipe` YAML format — a command also needs a
//! `slash_commands:` config.yaml entry, an agent a `sub_recipes:`/by-name
//! reference — with a Jinja `{{ }}` parameter contract the brief could not pin
//! down; writing a malformed recipe a live goose rejects is the risk we avoid).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};
use serde_norway::{Mapping, Value as Yaml};

use super::cchooks::{hook_is_portable, render_hook_group};
use super::confedit::{write_file_idem, yaml_edit};
use super::report;
use super::skillsdir;
use super::{AgentBackend, BackendState};
use crate::components::{HookBinding, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

/// goose's documented default extension timeout (seconds); written verbatim so a
/// re-render is byte-stable. We own the key, so overwriting a user's edited timeout
/// under *our* extension on the next reconcile is intended, not clobbering.
const DEFAULT_TIMEOUT: u32 = 300;

pub(crate) struct GooseBackend;

impl AgentBackend for GooseBackend {
    fn id(&self) -> &'static str {
        "goose"
    }

    fn detect(&self) -> bool {
        // `~/.config/goose` is XDG-based, so a test redirecting `XDG_CONFIG_HOME`
        // (or `HOME`) redirects detection too; the `goose` CLI on PATH is a bonus.
        which::which("goose").is_ok() || goose_config_dir().is_some_and(|c| c.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // user-only: MCP (the ownership anchor) lives in the single user
        // `config.yaml`; goose has no project-level extensions file. Hooks do have a
        // project variant, but v1 stays user-scope-primary so probe/remove key on
        // one coherent surface. mcp + hooks + skills translate; commands/agents are
        // skipped (goose's recipe format, module doc).
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: true,
            commands: false,
            agents: false,
            skills: true,
            instructions: false,
            scopes: &["user"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Compose the two surfaces (mcp extensions + the plugin-owned hooks.json), so
        // a missing hooks file behind healthy extensions reads NeedsRepair. `probe_mcp`
        // still carries the Disabled classification for a user-flipped `enabled:false`.
        // `source` is the one self_heal resolved for this agent (rehydrated `--path`,
        // else the compile-time default), so probe and reconcile render identical bytes.
        let comp = plugin.components(source)?;
        let mcp = if comp.mcp_servers.iter().any(is_writable) { Some(probe_mcp(&config_yaml()?, &comp.mcp_servers)?) } else { None };
        let hooks = probe_hooks(&hooks_json_path(scope, plugin.name)?, &comp.hooks)?;
        let skills = skillsdir::probe(&skills_dir(scope, plugin.name)?, plugin, &comp.skills)?;
        Ok(report::compose([mcp, hooks, skills].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;

        let mut changed = false;
        changed |= reconcile_mcp(&config_yaml()?, &comp.mcp_servers, desired.reenable)?;
        changed |= reconcile_hooks(&hooks_json_path(scope, plugin.name)?, &comp.hooks)?;
        changed |= skillsdir::reconcile(&skills_dir(scope, plugin.name)?, plugin, &comp.skills)?;
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
        let comp = plugin.components(source)?;

        let mut changed = false;
        changed |= remove_mcp(&config_yaml()?, &writable_names(&comp.mcp_servers))?;
        // The whole plugin dir under `~/.agents/plugins/<plugin>/` is ours (keyed by
        // our plugin name), so a wholesale drop removes only what we wrote.
        changed |= remove_plugin_dir(&plugin_dir(scope, plugin.name)?)?;
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// `GOOSE_PATH_ROOT`, goose's own path-root override: it wins unconditionally over
/// `XDG_CONFIG_HOME`/`HOME` (checked first, no merge) and relocates both the config
/// file and the plugins/hooks dir (`docs/harness/goose.md` gotcha 3). An unset or
/// empty value is ignored; goose treats it as an absolute root, so we don't
/// second-guess it here.
fn goose_path_root() -> Option<PathBuf> {
    std::env::var_os("GOOSE_PATH_ROOT").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The user-scope `~/.config/goose` dir (XDG-honoring), used only when
/// `GOOSE_PATH_ROOT` is unset. `detect()` doesn't consult `GOOSE_PATH_ROOT`, so a
/// test redirecting `XDG_CONFIG_HOME`/`HOME` still redirects detection.
fn goose_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|c| c.join("goose"))
}

/// The single `config.yaml` we read-modify-write for MCP. goose has exactly one
/// user-level config file (no project variant), so this takes no scope. Under
/// `GOOSE_PATH_ROOT` goose resolves `<root>/config/config.yaml` — a different layout
/// than `<XDG_CONFIG_HOME>/goose/config.yaml`, not a `.join("goose")` on the root. A
/// missing config home is a clear, actionable error rather than a silent skip.
fn config_yaml() -> Result<PathBuf> {
    if let Some(root) = goose_path_root() {
        return Ok(root.join("config").join("config.yaml"));
    }
    goose_config_dir()
        .map(|d| d.join("config.yaml"))
        .ok_or_else(|| Error::Tree("no config directory (HOME and XDG_CONFIG_HOME unset); cannot locate ~/.config/goose".into()))
}

/// The plugin-owned dir under the Open Plugins hooks spec: `~/.agents/plugins/
/// <plugin>/` (user) or `<project>/.agents/plugins/<plugin>/` (project). User scope
/// honors `GOOSE_PATH_ROOT` first (goose relocates its plugins dir under the same
/// root as the config file), then falls back to the ordinary HOME-based dir, so a
/// test redirecting either env redirects it. The whole dir is ours.
fn plugin_dir(scope: &Scope, plugin: &str) -> Result<PathBuf> {
    let base = match scope {
        Scope::User => match goose_path_root() {
            Some(root) => root,
            None => {
                dirs::home_dir().ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.agents/plugins".into()))?
            }
        },
        Scope::Project { path } => path.clone(),
    };
    Ok(base.join(".agents").join("plugins").join(plugin))
}

fn hooks_json_path(scope: &Scope, plugin: &str) -> Result<PathBuf> {
    Ok(plugin_dir(scope, plugin)?.join("hooks").join("hooks.json"))
}

/// The skills root inside the plugin dir goose owns: `<plugin_dir>/skills`. goose
/// auto-discovers `~/.agents/plugins/<plugin>/skills/<name>/SKILL.md`. `remove` drops
/// the whole plugin dir, so skills need no separate removal.
fn skills_dir(scope: &Scope, plugin: &str) -> Result<PathBuf> {
    Ok(plugin_dir(scope, plugin)?.join("skills"))
}

/// What goose can faithfully host: stdio and streamable HTTP. An `sse` extension
/// deserializes, then goose refuses it at runtime ("SSE is unsupported, migrate to
/// streamable_http") — a permanently dead entry — so sse is skipped exactly like a
/// non-portable server: never written, never owned, never removed.
fn is_writable(server: &McpServer) -> bool {
    server.is_portable() && !matches!(server.kind, McpKind::Sse { .. })
}

/// Server names `reconcile_mcp` actually writes. `remove` and doctor key off the
/// same set so a user server sharing a name with one we declared but never wrote
/// (non-portable, or sse) is never touched or flagged.
fn writable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| is_writable(s)).map(|s| s.name.as_str()).collect()
}

// --- mcp (goose extensions) --------------------------------------------------

/// A goose stdio extension entry. Field names are goose's own (`cmd`/`args`/`envs`,
/// not CC's `command`/`env`); `name` matches the map key as goose's own writer does.
#[derive(Serialize)]
struct StdioExt<'a> {
    name: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    cmd: &'a str,
    args: &'a [String],
    envs: &'a BTreeMap<String, String>,
    enabled: bool,
    timeout: u32,
}

/// A goose remote (streamable_http/sse) extension entry — keyed by `uri`, not
/// `url`. Best-effort: the tested fixture path is stdio, remotes ride the same
/// deterministic render.
#[derive(Serialize)]
struct RemoteExt<'a> {
    name: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    uri: &'a str,
    enabled: bool,
    timeout: u32,
}

/// Render one extension body as a YAML value. Deterministic per `enabled` so a
/// re-reconcile is structurally identical -> a true `NoOp`.
fn render_ext(server: &McpServer, enabled: bool) -> Result<Yaml> {
    let value = match &server.kind {
        McpKind::Stdio => serde_norway::to_value(StdioExt {
            name: &server.name,
            kind: "stdio",
            cmd: &server.command,
            args: &server.args,
            envs: &server.env,
            enabled,
            timeout: DEFAULT_TIMEOUT,
        }),
        // goose calls streamable HTTP `streamable_http`.
        McpKind::Http { url } => {
            serde_norway::to_value(RemoteExt { name: &server.name, kind: "streamable_http", uri: url, enabled, timeout: DEFAULT_TIMEOUT })
        }
        // Unreachable through reconcile (the `is_writable` filter skips sse); kept
        // as a hard error so a future caller can't write a dead extension.
        McpKind::Sse { .. } => {
            return Err(Error::Config {
                path: "<goose extension>".into(),
                detail: format!("goose cannot host an SSE extension ({}); it must be skipped, not rendered", server.name),
            });
        }
    };
    value.map_err(|e| Error::Config { path: "<goose extension>".into(), detail: format!("rendering extension: {e}") })
}

/// Ensure `root["extensions"]` is a mapping and return it. `root` is a mapping by
/// the `yaml_edit` contract; we own the `extensions` namespace, so replacing a
/// non-mapping value there is intended, not clobbering.
fn ext_map(root: &mut Yaml) -> &mut Mapping {
    let Yaml::Mapping(map) = root else { unreachable!("yaml_edit guarantees a mapping root") };
    let entry = map.entry(Yaml::from("extensions")).or_insert_with(|| Yaml::Mapping(Mapping::new()));
    if !entry.is_mapping() {
        *entry = Yaml::Mapping(Mapping::new());
    }
    let Yaml::Mapping(exts) = entry else { unreachable!("just ensured a mapping") };
    exts
}

/// Insert/update exactly our extensions under the `extensions` map, leaving the
/// user's own keys. Skips the write entirely (no empty `extensions` key) when the
/// plugin declares no portable server. `reenable=false` (self_heal) preserves an
/// existing explicit `enabled: false` on our own key instead of forcing it back on
/// (goose's `enabled` is a real per-extension disable a user can set; the
/// never-re-enable invariant applies to it exactly like CC's plugin disable).
/// `reenable=true` (an explicit install/update) always re-enables.
fn reconcile_mcp(config: &Path, servers: &[McpServer], reenable: bool) -> Result<bool> {
    let portable: Vec<&McpServer> = servers.iter().filter(|s| is_writable(s)).collect();
    if portable.is_empty() {
        return Ok(false);
    }
    yaml_edit(config, |root| {
        let exts = ext_map(root);
        for server in &portable {
            let currently_disabled = exts.get(server.name.as_str()).and_then(|v| v.get("enabled")).and_then(Yaml::as_bool) == Some(false);
            let enabled = reenable || !currently_disabled;
            exts.insert(Yaml::from(server.name.clone()), render_ext(server, enabled)?);
        }
        Ok(())
    })
}

/// Remove exactly our extension keys under `extensions`, leaving others.
/// Conservatively leaves an emptied `extensions` mapping in place rather than
/// dropping the file (a user may have unrelated top-level keys/comments).
fn remove_mcp(config: &Path, names: &[&str]) -> Result<bool> {
    if !config.exists() || names.is_empty() {
        return Ok(false);
    }
    yaml_edit(config, |root| {
        if let Some(exts) = root.as_mapping_mut().and_then(|m| m.get_mut("extensions")).and_then(Yaml::as_mapping_mut) {
            for name in names {
                exts.remove(*name);
            }
        }
        Ok(())
    })
}

/// `Absent` if none of our extensions are present; `Disabled` if all present ones
/// exactly match our render with `enabled: false` (a user's deliberate goose-level
/// disable self_heal must never flip back); `Healthy` if all present and matching
/// our enabled render; `NeedsRepair` otherwise (drifted or a mix). `Healthy` (not
/// `Absent`) when the plugin declares no portable server, so a present marker is
/// never dropped for an mcp-less plugin.
fn probe_mcp(config: &Path, servers: &[McpServer]) -> Result<BackendState> {
    // Checked before the file read: a plugin declaring no portable server writes no
    // `config.yaml`, so a missing file must still be `Healthy` (never `Absent`, which
    // self_heal maps to marker-drop) — missing- and empty-file must agree here.
    let portable: Vec<&McpServer> = servers.iter().filter(|s| is_writable(s)).collect();
    if portable.is_empty() {
        return Ok(BackendState::Healthy);
    }
    let bytes = match fs::read(config) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BackendState::Absent),
        Err(source) => return Err(Error::Io { context: format!("reading {}", config.display()), source }),
    };
    // An empty/whitespace-only file is the same as missing: no extensions present.
    let root: Yaml = if bytes.iter().all(u8::is_ascii_whitespace) {
        Yaml::Mapping(Mapping::new())
    } else {
        serde_norway::from_slice(&bytes).map_err(|e| Error::Config { path: config.display().to_string(), detail: e.to_string() })?
    };

    let exts = root.get("extensions");
    let mut present = 0usize;
    let mut enabled = 0usize;
    let mut disabled = 0usize;
    for server in &portable {
        if let Some(existing) = exts.and_then(|e| e.get(server.name.as_str())) {
            present += 1;
            if *existing == render_ext(server, true)? {
                enabled += 1;
            } else if *existing == render_ext(server, false)? {
                disabled += 1;
            }
        }
    }
    Ok(if present == 0 {
        BackendState::Absent
    } else if disabled == portable.len() {
        BackendState::Disabled
    } else if enabled == portable.len() {
        BackendState::Healthy
    } else {
        BackendState::NeedsRepair
    })
}

// --- hooks (plugin-owned dir) ------------------------------------------------

/// goose names its hook events after Claude Code's (the cross-tool "Open Plugins"
/// spec), extending them with its own file/shell events CC never emits. A CC event
/// goose also names passes through 1:1; one goose does not define is skipped rather
/// than written under a guess.
const GOOSE_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "Stop",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "BeforeReadFile",
    "AfterFileEdit",
    "BeforeShellExecution",
    "AfterShellExecution",
];

fn map_event(cc_event: &str) -> Option<&'static str> {
    GOOSE_EVENTS.iter().copied().find(|e| *e == cc_event)
}

/// Build the full `hooks.json` bytes from our portable, goose-named hooks (CC's
/// exact shape). `None` when there is nothing to write, so an empty plugin dir is
/// never created. We own the whole dir, so this is a wholesale render (no merge);
/// deterministic bytes make `write_file_idem` a true `NoOp` on the second pass.
fn build_hooks_json(hooks: &[HookBinding]) -> Result<Option<Vec<u8>>> {
    let writable: Vec<(&'static str, &HookBinding)> =
        hooks.iter().filter(|h| hook_is_portable(h)).filter_map(|h| map_event(&h.event).map(|e| (e, h))).collect();
    if writable.is_empty() {
        return Ok(None);
    }
    let mut events = Map::new();
    for (event, hook) in &writable {
        let entry = events.entry((*event).to_string()).or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(list) = entry {
            list.push(render_hook_group(hook));
        }
    }
    let mut root = Map::new();
    root.insert("hooks".into(), Value::Object(events));
    let mut bytes =
        serde_json::to_vec_pretty(&Value::Object(root)).map_err(|source| Error::Json { what: "goose hooks.json".into(), source })?;
    bytes.push(b'\n');
    Ok(Some(bytes))
}

fn reconcile_hooks(hooks_json: &Path, hooks: &[HookBinding]) -> Result<bool> {
    match build_hooks_json(hooks)? {
        Some(bytes) => write_file_idem(hooks_json, &bytes),
        None => Ok(false),
    }
}

/// Classify the plugin-owned `hooks.json` for `probe`: `None` when there is nothing
/// to write (so the surface contributes no verdict), else the byte-match state of the
/// one file we render. Uses the same `build_hooks_json` bytes `reconcile` writes.
fn probe_hooks(hooks_json: &Path, hooks: &[HookBinding]) -> Result<Option<BackendState>> {
    match build_hooks_json(hooks)? {
        Some(bytes) => report::probe_files(&[(hooks_json.to_path_buf(), bytes)], |_, _| true),
        None => Ok(None),
    }
}

/// Drop the whole plugin dir we own. Absent -> `false` (a plugin with no portable
/// hooks never created it).
fn remove_plugin_dir(dir: &Path) -> Result<bool> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(Error::Io { context: format!("removing {}", dir.display()), source }),
    }
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &GooseBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "goose detected", status: CheckStatus::Ok("`goose` on PATH or ~/.config/goose present".into()) }
    } else {
        DoctorCheck {
            name: "goose detected",
            status: CheckStatus::Fail {
                problem: "goose CLI not detected".into(),
                fix: "install it with the goose `download_cli.sh` installer".into(),
            },
        }
    });

    let config = match config_yaml() {
        Ok(config) => config,
        Err(e) => {
            checks.push(DoctorCheck { name: "config file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };

    let root = match fs::read(&config) {
        Ok(bytes) => match serde_norway::from_slice::<Yaml>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "config file", status: CheckStatus::Ok(format!("{} parses", config.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "config file",
                    status: CheckStatus::Fail {
                        problem: format!("{} does not parse: {e}", config.display()),
                        fix: "fix the YAML syntax or remove the file".into(),
                    },
                });
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            checks.push(DoctorCheck {
                name: "config file",
                status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", config.display())),
            });
            None
        }
        Err(e) => {
            checks
                .push(DoctorCheck { name: "config file", status: CheckStatus::Warn(format!("could not read {}: {e}", config.display())) });
            None
        }
    };

    let Some(comp) = report::components(&mut checks, plugin, source) else {
        return checks;
    };

    checks.push(check_mcp_registered(&comp.mcp_servers, root.as_ref()));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    match hooks_json_path(&Scope::User, plugin.name) {
        Ok(hooks_json) => checks.push(check_hooks_present(&comp.hooks, &hooks_json)),
        Err(e) => checks.push(DoctorCheck { name: "translated hooks present", status: CheckStatus::Warn(e.to_string()) }),
    }

    checks
}

fn check_mcp_registered(servers: &[McpServer], root: Option<&Yaml>) -> DoctorCheck {
    let name = "mcp extension registered";
    let portable: Vec<&str> = writable_names(servers);
    if portable.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no portable mcp servers to register".into()) };
    }
    let exts = root.and_then(|r| r.get("extensions"));
    let missing: Vec<&str> = portable.iter().copied().filter(|n| exts.and_then(|e| e.get(*n)).is_none()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} registered", portable.join(", "))) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp extension(s) not under `extensions` in config.yaml: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

/// goose hooks fire without a trust gate (unlike codex), so a present hook is a
/// plain `Ok`. A missing hook we should have written is a real `Fail`.
fn check_hooks_present(hooks: &[HookBinding], hooks_json: &Path) -> DoctorCheck {
    let name = "translated hooks present";
    let ours: Vec<&str> =
        hooks.iter().filter(|h| hook_is_portable(h) && map_event(&h.event).is_some()).map(|h| h.command.as_str()).collect();
    if ours.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no hooks to translate".into()) };
    }
    let text = fs::read_to_string(hooks_json).unwrap_or_default();
    let missing: Vec<&str> = ours.iter().copied().filter(|c| !text.contains(c)).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} hook(s) present in {}", ours.len(), hooks_json.display())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("hook(s) missing from hooks.json: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/goose.rs"]
mod goose_tests;
