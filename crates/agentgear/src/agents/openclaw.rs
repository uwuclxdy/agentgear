//! The openclaw backend: mcp-only, into openclaw's single user-level config
//! `~/.openclaw/openclaw.json`. MCP goes through the shared json renderer under the
//! two-segment key path `mcp.servers.<name>` with `ServerShape::plain()` (openclaw's
//! stdio body is exactly `{command, args, env}`). Every key is our own server name,
//! so `remove` is exact and a second reconcile is a true `NoOp`.
//!
//! openclaw exposes no config-writable surface for anything else agentgear
//! translates (see `docs/harness/openclaw.md`): hooks are JS/TS plugin code enabled
//! by a flag, never a shell command in JSON; "commands" are SKILL.md skills, not the
//! CC command-markdown shape; subagents live in-config but have no per-file surface
//! to point at. All are skipped. The config is JSON5 (comments + trailing commas
//! legal), but we parse it as strict JSON: a JSON5-only file surfaces as
//! `Error::Config` (the never-clobber path) rather than being silently rewritten.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::mcpjson::{self, ServerShape};
use super::report;
use super::{AgentBackend, BackendState};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

/// openclaw hosts mcp servers under `mcp.servers.<name>` (a two-segment key path,
/// unlike the json family's flat `mcpServers`).
const MCP_KEY: &[&str] = &["mcp", "servers"];

pub(crate) struct OpenclawBackend;

impl AgentBackend for OpenclawBackend {
    fn id(&self) -> &'static str {
        "openclaw"
    }

    fn detect(&self) -> bool {
        // The state dir is HOME-based (honoring `OPENCLAW_STATE_DIR`/`OPENCLAW_HOME`/
        // `OPENCLAW_CONFIG_PATH`), so a test redirecting any of them redirects
        // detection; the `openclaw` CLI on PATH is a bonus.
        which::which("openclaw").is_ok() || config_path_opt().is_some_and(|p| p.parent().is_some_and(Path::is_dir))
    }

    fn capabilities(&self) -> Capabilities {
        // `hooks:false` — openclaw's only hook surface is JS/TS plugin code toggled by
        // a config flag, not a shell command we can write. User scope only: the brief
        // documents no project-level config file.
        Capabilities { plugins: false, mcp: true, hooks: false, scopes: &["user"] }
    }

    fn probe(&self, plugin: &Plugin, _scope: &Scope) -> Result<BackendState> {
        // Ownership is defined by our mcp server keys (the canonical "are we here"
        // signal); the shared probe returns Healthy — never Absent — for an mcp-less
        // plugin, so a present marker is never dropped. Source::Embedded is the only
        // steady-state source for a non-CC backend (github unsupported, path is
        // install-only), mirroring the claude probe keying on compile-time metadata.
        let comp = plugin.components(&Source::Embedded)?;
        mcpjson::probe(&config_path()?, MCP_KEY, &comp.mcp_servers, ServerShape::plain())
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, _scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        mcpjson::reconcile(&config_path()?, MCP_KEY, &comp.mcp_servers, ServerShape::plain())
    }

    fn remove(&self, plugin: &Plugin, _scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        mcpjson::remove(&config_path()?, MCP_KEY, &comp.mcp_servers, ServerShape::plain())
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// openclaw's single user-level config file, honoring its documented env overrides
/// so a test (or a relocated install) redirects both detection and writes.
/// Precedence: `OPENCLAW_CONFIG_PATH` (the file itself) → `OPENCLAW_STATE_DIR` (flat:
/// `<dir>/openclaw.json`) → `OPENCLAW_HOME` (a HOME-equivalent: openclaw reads
/// `<dir>/.openclaw/openclaw.json`, one level below it) → `~/.openclaw/openclaw.json`.
/// `_opt` never errors so `detect` can call it. Scope is not consulted: the brief
/// documents no project-level config, so every scope maps to this one file.
fn config_path_opt() -> Option<PathBuf> {
    config_path_from(
        env_nonempty("OPENCLAW_CONFIG_PATH").map(PathBuf::from),
        env_nonempty("OPENCLAW_STATE_DIR").map(PathBuf::from),
        env_nonempty("OPENCLAW_HOME").map(PathBuf::from),
        dirs::home_dir(),
    )
}

/// Pure precedence resolver (no env reads), so the layout rules are unit-testable
/// without mutating process env: `config_path` wins outright; `state_dir` is flat;
/// `home_override` and the final `home` fallback both need the extra `.openclaw`
/// level openclaw itself reads.
fn config_path_from(
    config_path: Option<PathBuf>, state_dir: Option<PathBuf>, home_override: Option<PathBuf>, home: Option<PathBuf>,
) -> Option<PathBuf> {
    config_path
        .or_else(|| state_dir.map(|d| d.join("openclaw.json")))
        .or_else(|| home_override.map(home_config))
        .or_else(|| home.map(home_config))
}

fn home_config(home: PathBuf) -> PathBuf {
    home.join(".openclaw").join("openclaw.json")
}

fn config_path() -> Result<PathBuf> {
    config_path_opt().ok_or_else(|| {
        Error::Tree("no home dir (HOME/OPENCLAW_STATE_DIR/OPENCLAW_HOME/OPENCLAW_CONFIG_PATH unset); cannot locate ~/.openclaw".into())
    })
}

fn env_nonempty(var: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(var).filter(|v| !v.is_empty())
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &OpenclawBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "openclaw detected", status: CheckStatus::Ok("`openclaw` on PATH or ~/.openclaw present".into()) }
    } else {
        DoctorCheck {
            name: "openclaw detected",
            status: CheckStatus::Fail {
                problem: "openclaw CLI not detected".into(),
                fix: "install it with `curl -fsSL https://openclaw.ai/install.sh | bash`".into(),
            },
        }
    });

    let config = match config_path() {
        Ok(config) => config,
        Err(e) => {
            checks.push(DoctorCheck { name: "config file", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };

    let root = match fs::read(&config) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "config file", status: CheckStatus::Ok(format!("{} parses", config.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "config file",
                    status: CheckStatus::Fail {
                        // JSON5-only syntax (comments/trailing commas) trips the strict
                        // parser here too — we never overwrite what we can't parse.
                        problem: format!("{} does not parse as strict JSON: {e}", config.display()),
                        fix: "remove JSON5-only comments/trailing commas or the file".into(),
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

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcp", "servers"],
        "not under `mcp.servers` in openclaw.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));

    checks
}

#[cfg(test)]
#[path = "../../tests/unit/openclaw.rs"]
mod openclaw_tests;
