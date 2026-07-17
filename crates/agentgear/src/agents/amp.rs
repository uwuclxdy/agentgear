//! The amp (Sourcegraph Amp) backend: mcp-only, into amp's own `settings.json`.
//! Amp keys its servers under a single VS-Code-style flat key `"amp.mcpServers"`
//! — a literal dotted string, not a nested `amp` object — so the shared json
//! renderer targets a one-segment key path of exactly that string. Amp's stdio
//! body (`{command,args,env}`) is byte-identical to the shared `Plain` shape, so
//! the whole json family's atomic, merge-safe reconcile is reused verbatim: only
//! our server keys are touched, the user's survive, and a second reconcile is a
//! true `NoOp`.
//!
//! Skipped surfaces (see `docs/harness/amp.md`): hooks, slash-commands, and
//! subagents have **no file-writable surface** in amp — its only lifecycle /
//! command / agent mechanism is an in-process TypeScript plugin API
//! (`~/.config/amp/plugins/*.ts`), an executable program rather than declarative
//! config, so nothing translates. Skills are out of v1 scope. Amp also reads a
//! comment-bearing `settings.jsonc`, which `json_edit` cannot parse or merge and
//! whose precedence over a plain `settings.json` amp does not document; if one
//! exists, `reconcile` refuses with `Error::Config` rather than writing a
//! `settings.json` that would shadow it or silently never load. Amp's remote
//! (http/sse) form omits the `type` discriminator the shared renderer emits,
//! inferring transport from `url`, a redundant-but-harmless field on the untested
//! remote path.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct AmpBackend;

/// Amp's server map lives under one literal, VS-Code-style flat key: a single
/// key-path segment that itself contains a dot, not a nested `amp` object.
const MCP_KEY: &[&str] = &["amp.mcpServers"];

impl AgentBackend for AmpBackend {
    fn id(&self) -> &'static str {
        "amp"
    }

    fn detect(&self) -> bool {
        // Amp resolves `$XDG_CONFIG_HOME/amp` (else `~/.config/amp`) on every
        // platform, not the OS config dir that diverges on macOS/Windows, so a test
        // redirecting `XDG_CONFIG_HOME` (or `HOME`) also redirects detection. Amp
        // documents no other config-dir override env, so the `amp` binary on PATH
        // is the only other signal.
        which::which("amp").is_ok() || user_config_dir().is_some_and(|d| d.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // `hooks:false` — amp's only hook/command/agent surface is an in-process TS
        // plugin API, not the declarative config a backend can write (module doc).
        Capabilities { plugins: false, mcp: true, hooks: false, scopes: &["user"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        // Ownership is defined by our mcp server keys (the canonical "are we here"
        // signal); the shared probe returns Healthy — never Absent — for an mcp-less
        // plugin, so a present marker is never dropped. Source::Embedded is the only
        // steady-state source for a non-CC backend (github unsupported, path is
        // install-only), mirroring the claude probe keying on compile-time metadata.
        let comp = plugin.components(&Source::Embedded)?;
        probe_mcp(&settings_path(scope)?, &comp.mcp_servers)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        // `reenable` is unused: amp's mcp entry has no per-server disable flag (the
        // Plain shape carries none), so there is nothing self_heal could re-enable.
        let comp = plugin.components(&desired.source)?;
        reconcile_mcp(&settings_path(scope)?, &comp.mcp_servers)
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        remove_mcp(&settings_path(scope)?, &comp.mcp_servers)
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// Amp's user config dir: `$XDG_CONFIG_HOME/amp` or `~/.config/amp` on every
/// platform (amp hardcodes this literal path, not the OS config dir that diverges
/// on macOS/Windows — so this replicates dirs' own XDG-or-home logic rather than
/// deferring to `dirs::config_dir()`).
fn user_config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .map(|c| c.join("amp"))
}

/// The amp config base for a scope. A missing config home is a clear, actionable
/// error. Project scope targets amp's own `.amp/` dir (not advertised in
/// `capabilities` — a project server needs an explicit `amp mcp approve`, so the
/// orchestrator only ever reaches user scope; this arm stays defensively correct).
fn amp_dir(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => user_config_dir()
            .ok_or_else(|| Error::Tree("no config directory (XDG_CONFIG_HOME and HOME both unset); cannot locate ~/.config/amp".into())),
        Scope::Project { path } => Ok(path.join(".amp")),
    }
}

fn settings_path(scope: &Scope) -> Result<PathBuf> {
    Ok(amp_dir(scope)?.join("settings.json"))
}

/// A sibling `settings.jsonc` amp also reads. `json_edit` cannot parse or merge
/// its comments, and amp does not document whether a plain `settings.json` takes
/// precedence, so if one exists `reconcile` refuses rather than write a shadowing
/// `settings.json` that would clobber the user's config or silently never load.
fn jsonc_sibling(settings: &Path) -> Option<PathBuf> {
    let jsonc = settings.with_extension("jsonc");
    jsonc.is_file().then_some(jsonc)
}

// --- mcp (shared json renderer, amp's flat key + Plain body) -----------------

/// Bind amp's `(flat key, Plain shape)` in one place so the trait methods and the
/// unit tests reference the same choice — a key/shape change is caught by the test.
fn reconcile_mcp(settings: &Path, servers: &[McpServer]) -> Result<Outcome> {
    if let Some(jsonc) = jsonc_sibling(settings) {
        return Err(Error::Config {
            path: jsonc.display().to_string(),
            detail: "amp reads this comment-bearing settings.jsonc, which agentgear cannot parse or merge; refusing to \
                     write a shadowing settings.json. consolidate into one plain settings.json (drop the comments) and re-run setup"
                .into(),
        });
    }
    mcpjson::reconcile(settings, MCP_KEY, servers, ServerShape::plain())
}

fn probe_mcp(settings: &Path, servers: &[McpServer]) -> Result<BackendState> {
    mcpjson::probe(settings, MCP_KEY, servers, ServerShape::plain())
}

fn remove_mcp(settings: &Path, servers: &[McpServer]) -> Result<Outcome> {
    mcpjson::remove(settings, MCP_KEY, servers, ServerShape::plain())
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &AmpBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "amp detected", status: CheckStatus::Ok("`amp` on PATH or ~/.config/amp present".into()) }
    } else {
        DoctorCheck {
            name: "amp detected",
            status: CheckStatus::Fail {
                problem: "amp CLI not detected".into(),
                fix: "install it with `curl -fsSL https://ampcode.com/install.sh | bash`".into(),
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

    let root = match fs::read(&settings) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "settings file", status: CheckStatus::Ok(format!("{} parses", settings.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "settings file",
                    status: CheckStatus::Fail {
                        problem: format!("{} does not parse: {e}", settings.display()),
                        // A comment-bearing settings.jsonc is valid to amp but not to us: never clobbered, but not merged.
                        fix: "fix the JSON syntax; agentgear writes plain settings.json and cannot merge a comment-bearing settings.jsonc"
                            .into(),
                    },
                });
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A sibling comment-bearing `settings.jsonc` is config amp reads but the
            // backend cannot merge, so `reconcile` refuses rather than shadow it; name
            // that here instead of a bare "run setup" that would loop the user.
            let status = match jsonc_sibling(&settings) {
                Some(jsonc) => CheckStatus::Warn(format!(
                    "{} is comment-bearing; agentgear writes plain settings.json and cannot merge it, so setup refuses rather than shadow it",
                    jsonc.display()
                )),
                None => CheckStatus::Warn(format!("{} does not exist yet (run setup)", settings.display())),
            };
            checks.push(DoctorCheck { name: "settings file", status });
            None
        }
        Err(e) => {
            checks.push(DoctorCheck {
                name: "settings file",
                status: CheckStatus::Warn(format!("could not read {}: {e}", settings.display())),
            });
            None
        }
    };

    let comp = match plugin.components(source) {
        Ok(comp) => comp,
        Err(e) => {
            checks.push(DoctorCheck {
                name: "plugin components",
                status: CheckStatus::Fail {
                    problem: format!("could not read the plugin tree: {e}"),
                    fix: "rebuild the host binary".into(),
                },
            });
            return checks;
        }
    };

    checks.push(check_mcp_registered(&comp.mcp_servers, root.as_ref()));
    checks.push(check_mcp_command(&comp.mcp_servers));

    checks
}

fn check_mcp_registered(servers: &[McpServer], root: Option<&Value>) -> DoctorCheck {
    let name = "mcp server registered";
    let portable: Vec<&str> = servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect();
    if portable.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no portable mcp servers to register".into()) };
    }
    // The literal flat key, looked up as one string (not a nested `amp` object).
    let obj = root.and_then(|r| r.get("amp.mcpServers")).and_then(Value::as_object);
    let missing: Vec<&str> = portable.iter().copied().filter(|n| obj.is_none_or(|o| !o.contains_key(*n))).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} registered", portable.join(", "))) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp server(s) not under `amp.mcpServers` in settings.json: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

fn check_mcp_command(servers: &[McpServer]) -> DoctorCheck {
    let name = "mcp command on PATH";
    let missing: Vec<String> = servers
        .iter()
        .filter(|s| s.is_portable() && matches!(s.kind, McpKind::Stdio))
        .map(|s| s.command.clone())
        // Only a bare executable name is a PATH lookup; a path/variable command can't be checked generically.
        .filter(|c| !c.is_empty() && !c.contains('/') && !c.contains('\\') && !c.contains('$') && which::which(c).is_err())
        .collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok("all referenced mcp commands resolve".into()) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp command(s) not on PATH: {}", missing.join(", ")),
                fix: "install the missing binaries into a PATH directory".into(),
            },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/amp.rs"]
mod amp_tests;
