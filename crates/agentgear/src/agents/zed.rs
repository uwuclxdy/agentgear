//! The zed backend — mcp + skills + instructions. Zed reads MCP servers from a
//! `context_servers` object in its `settings.json` (`~/.config/zed/settings.json`,
//! XDG-based on both Linux and macOS; `%APPDATA%\Zed` on Windows): stdio is the
//! shared Plain `{command,args,env}` body, remote http is `{url,headers}` (zed's
//! one remote transport; sse is skipped — no faithful landing). Entries are keyed
//! by our plugin's server names, so `remove` is exact and a second reconcile is a
//! true `NoOp`. Skills land as bare `<name>/SKILL.md` under zed's ONLY skill path
//! `~/.agents/skills` (user) / `<worktree>/.agents/skills` (project), tagged for
//! ownership so a foreign skill in that shared root is never swept; zed requires
//! `name`+`description`, which the shared renderer ensures. The host's
//! always-loaded guidance lands as a tagged block inside zed's own
//! `~/.config/zed/AGENTS.md` (user scope only — the worktree-root `AGENTS.md` is
//! the repo's own file, never written), marker-id-scoped so the user's lines and
//! a rival plugin's block survive byte-identical. Accepted limit: zed caps
//! the aggregate name+description catalog at ~50KB — a plugin shipping past that is
//! the author's concern, not truncated here. Zed ships no general hook/command/
//! subagent config-file surface (its `tasks.json` "hooks" fire on a single
//! `create_worktree` event, not a CC lifecycle), so those components are skipped.

use std::path::{Path, PathBuf};

use super::agentsmd;
use super::mcpjson::{self, RemoteShape, ServerShape};
use super::report;
use super::skillsdir;
use super::{AgentBackend, BackendState};
use crate::components::McpServer;
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct ZedBackend;

impl AgentBackend for ZedBackend {
    fn id(&self) -> &'static str {
        "zed"
    }

    fn detect(&self) -> bool {
        // The upstream CLI is `zed` (some distros rename it to `zedit`/`zeditor`, but
        // the default name is authoritative); the config dir is XDG-based on both
        // Linux and macOS, so a test redirecting `XDG_CONFIG_HOME`/`HOME` also
        // redirects detection. Zed has no user-config-dir override env of its own.
        which::which("zed").is_ok() || user_config_dir().is_some_and(|d| d.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // mcp + skills + user-scope instructions: zed has no config-file
        // hook/command/subagent surface to translate.
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: false,
            commands: false,
            agents: false,
            skills: true,
            instructions: true,
            scopes: &["user", "project"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Ownership is defined by our mcp server keys (the canonical "are we here"
        // signal); the shared probe returns Healthy — never Absent — for a plugin with
        // no writable servers, so a present marker is never dropped. `source` is the one
        // self_heal resolved for this agent (rehydrated `--path`, else the compile-time
        // default), so probe and reconcile render identical bytes.
        let comp = plugin.components(source)?.with_client(self.id());
        let mcp = probe_mcp(&settings_path(scope)?, &comp.mcp_servers)?;
        let skills = skillsdir::probe(&skillsdir::agents_skills_root(scope)?, plugin, &comp.skills)?;
        let instructions = match agents_md(scope)? {
            Some(path) => agentsmd::probe(&path, plugin)?,
            None => None,
        };
        Ok(report::compose([Some(mcp), skills, instructions].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?.with_client(self.id());
        let mut changed = reconcile_mcp(&settings_path(scope)?, &comp.mcp_servers)? != Outcome::NoOp;
        changed |= skillsdir::reconcile(&skillsdir::agents_skills_root(scope)?, plugin, &comp.skills)?;
        if let Some(path) = agents_md(scope)? {
            changed |= agentsmd::reconcile(&path, plugin)?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome> {
        let comp = plugin.components(source)?.with_client(self.id());
        let mut changed = remove_mcp(&settings_path(scope)?, &comp.mcp_servers)? != Outcome::NoOp;
        changed |= skillsdir::remove(&skillsdir::agents_skills_root(scope)?, plugin, &comp.skills)?;
        if let Some(path) = agents_md(scope)? {
            changed |= agentsmd::remove(&path, plugin)?;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// Zed's user config dir, per its own `paths.rs`: `%APPDATA%\Zed` on Windows;
/// `$XDG_CONFIG_HOME/zed` else `~/.config/zed` on Linux/FreeBSD; and on macOS a
/// hardcoded `~/.config/zed` with NO env consulted — the location matches the XDG
/// default but the mechanism doesn't, so an exported `XDG_CONFIG_HOME` must NOT
/// redirect the macOS arm (zed wouldn't follow it).
fn user_config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        dirs::config_dir().map(|c| c.join("Zed"))
    }
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir().map(|h| h.join(".config").join("zed"))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        xdg_or_home_config(std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from), dirs::home_dir())
    }
}

/// The Linux/FreeBSD arm's pure half: XDG (absolute, non-empty) wins, else
/// `~/.config`, joined `zed`. Split from the env read so a unit test pins the
/// precedence without mutating process env.
#[cfg(not(any(windows, target_os = "macos")))]
fn xdg_or_home_config(xdg: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    xdg.filter(|p| p.is_absolute()).or_else(|| home.map(|h| h.join(".config"))).map(|c| c.join("zed"))
}

/// The `settings.json` for a scope: the user config dir (a missing config dir is a
/// clear, actionable error, never a silent write to the wrong place) or the
/// worktree-local `<cwd>/.zed/settings.json` a project override lives in.
fn settings_path(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => user_config_dir().map(|d| d.join("settings.json")).ok_or_else(|| {
            Error::Tree("no config directory (XDG_CONFIG_HOME and HOME both unset); cannot locate zed's settings.json".into())
        }),
        Scope::Project { path } => Ok(path.join(".zed").join("settings.json")),
    }
}

/// The user-scope `~/.config/zed/AGENTS.md` carrying the host's always-loaded
/// guidance as a tagged block. Project scope has none: the worktree-root
/// `AGENTS.md` is the repo's own file, and writing it would clobber user content.
fn agents_md(scope: &Scope) -> Result<Option<PathBuf>> {
    match scope {
        Scope::User => Ok(Some(
            user_config_dir()
                .ok_or_else(|| {
                    Error::Tree("no config directory (XDG_CONFIG_HOME and HOME both unset); cannot locate zed's AGENTS.md".into())
                })?
                .join("AGENTS.md"),
        )),
        Scope::Project { .. } => Ok(None),
    }
}

// --- mcp ---------------------------------------------------------------------

/// Zed's MCP servers live under this single top-level key.
const CONTEXT_SERVERS: &str = "context_servers";
const MCP_KEY: &[&str] = &[CONTEXT_SERVERS];

/// Zed's one remote transport is streamable HTTP, modeled as `{url, headers}` with
/// no discriminator key; sse has no faithful landing (zed would dial the URL as
/// streamable HTTP against an SSE endpoint) and is skipped by the dialect.
const SHAPE: ServerShape = ServerShape::plain().with_remote(RemoteShape::UrlHeadersHttpOnly);

/// Insert/update our servers under `context_servers`, leaving the user's own.
/// `NoOp` when the file already matches.
fn reconcile_mcp(settings: &Path, servers: &[McpServer]) -> Result<Outcome> {
    mcpjson::reconcile(settings, MCP_KEY, servers, SHAPE)
}

/// Classify `context_servers` for our servers (Absent/Healthy/NeedsRepair).
fn probe_mcp(settings: &Path, servers: &[McpServer]) -> Result<BackendState> {
    mcpjson::probe(settings, MCP_KEY, servers, SHAPE)
}

/// Strip exactly our server keys from `context_servers`, leaving the user's.
fn remove_mcp(settings: &Path, servers: &[McpServer]) -> Result<Outcome> {
    mcpjson::remove(settings, MCP_KEY, servers, SHAPE)
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &ZedBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "zed detected", status: CheckStatus::Ok("`zed` on PATH or a zed config dir present".into()) }
    } else {
        DoctorCheck {
            name: "zed detected",
            status: CheckStatus::Fail {
                problem: "zed not detected".into(),
                fix: "install it with `curl -f https://zed.dev/install.sh | sh`".into(),
            },
        }
    });

    let settings = match settings_path(&Scope::User) {
        Ok(settings) => settings,
        Err(e) => {
            checks.push(DoctorCheck { name: "settings file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };

    let root = report::read_json_config(&mut checks, "settings file", &settings);

    let Some(comp) = report::components(&mut checks, plugin, source).map(|c| c.with_client(backend.id())) else {
        return checks;
    };

    let writable = mcpjson::writable_names(&comp.mcp_servers, SHAPE);
    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        &writable,
        root.as_ref(),
        MCP_KEY,
        "not in settings.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));

    if plugin.instructions.is_some() {
        checks.push(agentsmd::guidance_check(agents_md(&Scope::User), plugin));
    }

    checks
}

#[cfg(test)]
#[path = "../../tests/unit/zed.rs"]
mod zed_tests;
