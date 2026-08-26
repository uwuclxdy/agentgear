//! `doctor`: a structured, ordered health report. Every check carries a rendered
//! fix-hint, so a failing host binary tells the user what to do rather than
//! surfacing a stack trace (design §doctor).

use std::fmt;
#[cfg(feature = "claude")]
use std::path::PathBuf;

#[cfg(feature = "claude")]
use serde_json::Value;

// Everything below carrying this gate belongs to `claude_report`, which only the
// `claude` backend calls; the shared `doctor` fan-out and the report types stay
// ungated so every other backend still reaches them.
#[cfg(feature = "claude")]
use crate::agents::AgentBackend;
#[cfg(feature = "claude")]
use crate::agents::claude::{ClaudeBackend, MarketplaceHealth, find_marketplace, marketplace_health};
#[cfg(feature = "claude")]
use crate::cli::{CLAUDE_FLOOR, ClaudeCli, MIN_CLAUDE_VERSION, parse_version};
use crate::components::AGENTGEAR_CLIENT_TOKEN;
use crate::error::Result;
#[cfg(feature = "claude")]
use crate::host::data_root;
use crate::host::{Plugin, Scope, Source};
#[cfg(feature = "claude")]
use crate::materialize::{dir_hash, dir_hash_for_client, tree_hash};

/// The CC client id materialize scopes its tree under. doctor is CC-oriented (it runs
/// `claude plugin validate` and hashes against the tree CC would run), so it reads and
/// hashes the `@claude`-scoped materialization rather than any other backend's.
#[cfg(feature = "claude")]
const CLAUDE_CLIENT: &str = "claude";

/// An ordered health report: one [`DoctorCheck`] per thing inspected, rendered by
/// its [`Display`](std::fmt::Display) into a `[ ok ]`/`[warn]`/`[fail]` list.
#[derive(Debug, Clone)]
pub struct DoctorReport {
    checks: Vec<DoctorCheck>,
}

/// One inspected thing and how it fared.
#[derive(Debug, Clone)]
pub struct DoctorCheck {
    /// The check's label (e.g. `"claude version"`).
    pub name: &'static str,
    /// Its result.
    pub status: CheckStatus,
}

/// A single check's verdict. Only [`CheckStatus::Fail`] makes a report unhealthy.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CheckStatus {
    /// Passed; the string is the detail shown after `[ ok ]`.
    Ok(String),
    /// A tolerated concern (the report stays healthy); the string is the detail.
    Warn(String),
    /// Failed; carries both what broke and how to fix it.
    Fail {
        /// What went wrong.
        problem: String,
        /// The rendered fix-hint.
        fix: String,
    },
}

impl DoctorReport {
    /// The checks in report order.
    pub fn checks(&self) -> &[DoctorCheck] {
        &self.checks
    }

    /// True when no check failed (warnings are tolerated).
    pub fn is_healthy(&self) -> bool {
        !self.checks.iter().any(|c| matches!(c.status, CheckStatus::Fail { .. }))
    }

    /// Build a report from a check list. This is how a backend's
    /// [`report`](crate::AgentBackend::report) assembles its result — the in-crate
    /// backends and an out-of-crate [`AgentBackend`](crate::AgentBackend) impl
    /// both go through it (`checks` itself stays private so a report is always
    /// built whole, never mutated after the fact).
    pub fn from_checks(checks: Vec<DoctorCheck>) -> Self {
        Self { checks }
    }

    /// Collapse an error into a single-check failed report, for a backend whose
    /// `report` hit a failure before it could produce individual checks (e.g. an
    /// unreadable config). An external backend maps its own failures through
    /// [`Error::Backend`](crate::Error::Backend) here.
    pub fn from_error(err: crate::error::Error) -> Self {
        Self {
            checks: vec![DoctorCheck {
                name: "doctor",
                status: CheckStatus::Fail { problem: err.to_string(), fix: "see the error above".into() },
            }],
        }
    }
}

impl fmt::Display for DoctorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for check in &self.checks {
            match &check.status {
                CheckStatus::Ok(detail) => writeln!(f, "[ ok ] {}: {detail}", check.name)?,
                CheckStatus::Warn(detail) => writeln!(f, "[warn] {}: {detail}", check.name)?,
                CheckStatus::Fail { problem, fix } => {
                    writeln!(f, "[fail] {}: {problem}\n       fix: {fix}", check.name)?;
                }
            }
        }
        Ok(())
    }
}

/// The full health report: one shared "host binary on PATH" check, then every
/// configured agent's own `report` merged in (design §6). `PluginHost::doctor`
/// calls this; a claude-only host gets the host-binary check followed by the
/// claude backend's own report, in order.
pub(crate) fn doctor(plugin: &Plugin, source: &Source) -> Result<DoctorReport> {
    // Both openers are about the HOST's own authoring, not any one agent's state, so
    // they run once before the fan-out rather than inside a backend's report.
    let mut checks = vec![check_host_binary()];
    checks.extend(check_statusline_client(plugin));
    for id in plugin.agents {
        // An unresolvable id is a failed CHECK, never an aborted report: a health
        // command that throws away every check it already collected is useless
        // exactly when it is needed.
        let backend = match crate::install::resolve(id) {
            Ok(backend) => backend,
            Err(e) => {
                checks.push(DoctorCheck {
                    name: id,
                    status: CheckStatus::Fail {
                        problem: e.to_string(),
                        fix: "rebuild the host with this agent's cargo feature enabled".into(),
                    },
                });
                continue;
            }
        };
        if backend.detect() {
            // This agent's OWN marker settles its source (never a sibling's — the
            // per-agent-marker invariant); `source` is the caller's `DEFAULT_SOURCE`.
            // `doctor` (like self_heal) has no scope of its own, so it keys on the
            // same user-scope marker self_heal writes.
            let resolved = crate::stamp::resolve_source(plugin, &Scope::User, id, source.clone());
            // A github source has no local tree for a config-merge backend, so
            // install/update/self_heal all skip it (visible in their reports);
            // doctor mirrors that as a Warn instead of running per-surface checks
            // that would all fail against a tree that cannot exist locally.
            if matches!(resolved, Source::GitHub { .. }) && !backend.capabilities().plugins {
                checks.push(DoctorCheck {
                    name: id,
                    status: CheckStatus::Warn(
                        "github source: this backend needs a local tree (embedded or path) and is skipped by setup".into(),
                    ),
                });
                continue;
            }
            checks.extend(backend.report(plugin, &resolved).checks);
        } else {
            // A declared harness that isn't installed here is not a failure: it is
            // simply not this host's concern, exactly as install/self_heal skip it.
            checks.push(DoctorCheck { name: id, status: CheckStatus::Ok("not installed on this host; skipped".into()) });
        }
    }
    Ok(DoctorReport { checks })
}

/// The Claude backend's slice of the report: version floor, registry presence,
/// manifest validation, tree-hash integrity, hook commands on PATH. The shared
/// host-binary check lives in the fan-out, not here, so the merged claude-only
/// report is unchanged. Infallible — every check resolves to a status.
#[cfg(feature = "claude")]
pub(crate) fn claude_report(plugin: &Plugin, source: &Source) -> DoctorReport {
    let mut checks = Vec::new();

    let cli = match ClaudeCli::locate() {
        Ok(cli) => {
            checks.push(check_claude_version(&cli));
            Some(cli)
        }
        Err(_) => {
            checks.push(DoctorCheck {
                name: "claude on PATH",
                status: CheckStatus::Fail {
                    problem: "`claude` not found on PATH".into(),
                    fix: "install it with `npm install -g @anthropic-ai/claude-code`".into(),
                },
            });
            None
        }
    };

    if let Some(cli) = cli {
        check_registered(&cli, plugin, &mut checks);
        checks.push(check_marketplace(&cli, plugin, source));
        checks.push(check_validate(plugin, source));
    }

    // These are pure local checks (hash the tree, resolve hook commands on PATH,
    // read settings.json) and stay useful even when `claude` is missing, so they run
    // unconditionally. The statusLine check is absent entirely for a host that
    // declares none, rather than reporting on a surface nobody asked for.
    //
    // No `ensure_statusline_resolves` hoist needed here: `claude_report` is infallible
    // (returns a `DoctorReport`, never propagates an `Err`) and every CLI call above is
    // a read (`list`, `--version`), never a mutation, so an unresolvable config dir has
    // no partial state to strand. `statusline_check` already surfaces it as its own
    // check (a `Fail` for an empty override, see `statuslinejson::check`).
    checks.push(check_tree_hash(plugin, source));
    checks.push(check_hook_commands(plugin));
    checks.extend(crate::agents::claude::statusline_check(plugin));

    DoctorReport { checks }
}

fn check_host_binary() -> DoctorCheck {
    let name = "host binary on PATH";
    let exe = std::env::current_exe().ok();
    let basename = exe.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
    match basename {
        Some(bin) if which::which(&bin).is_ok() => DoctorCheck { name, status: CheckStatus::Ok(format!("`{bin}` resolves on PATH")) },
        Some(bin) => DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("`{bin}` is not on PATH, so the plugin's hooks cannot invoke it"),
                fix: "install the binary into a PATH directory (e.g. `cargo install` or a package)".into(),
            },
        },
        None => DoctorCheck { name, status: CheckStatus::Warn("could not resolve the current executable name".into()) },
    }
}

#[cfg(feature = "claude")]
fn check_claude_version(cli: &ClaudeCli) -> DoctorCheck {
    let name = "claude version";
    let raw = match cli.raw_version() {
        Ok(v) => v,
        Err(e) => return DoctorCheck { name, status: CheckStatus::Warn(format!("could not read `claude --version`: {e}")) },
    };
    match parse_version(&raw) {
        Some(v) if v < CLAUDE_FLOOR => DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("`claude` {raw} is below the required floor {MIN_CLAUDE_VERSION}"),
                fix: "upgrade with `npm install -g @anthropic-ai/claude-code`".into(),
            },
        },
        Some(_) => DoctorCheck { name, status: CheckStatus::Ok(raw) },
        None => DoctorCheck { name, status: CheckStatus::Warn(format!("could not parse version {raw:?}; proceeding")) },
    }
}

#[cfg(feature = "claude")]
fn check_registered(cli: &ClaudeCli, plugin: &Plugin, checks: &mut Vec<DoctorCheck>) {
    let name = "plugin registered";
    let entries: Result<Vec<crate::manifest::PluginEntry>> = cli.run_json(&["plugin", "list", "--json"], None, "plugin list --json");
    match entries {
        Err(e) => {
            checks.push(DoctorCheck {
                name,
                status: CheckStatus::Fail {
                    problem: format!("`plugin list --json` did not parse: {e}"),
                    fix: "re-run `claude plugin list --json` and report the output".into(),
                },
            });
        }
        Ok(entries) => match entries.into_iter().find(|e| e.matches(plugin.name, plugin.marketplace)) {
            Some(entry) => {
                let enabled = entry.enabled.unwrap_or(true);
                let version = entry.version.clone().unwrap_or_else(|| "?".into());
                let state = if enabled { "enabled" } else { "disabled" };
                checks.push(DoctorCheck { name, status: CheckStatus::Ok(format!("{} v{version} ({state})", plugin.id())) });
            }
            None => {
                checks.push(DoctorCheck {
                    name,
                    status: CheckStatus::Fail {
                        problem: format!("{} is not installed", plugin.id()),
                        fix: "run the host binary's `setup` (or `install`) subcommand".into(),
                    },
                });
            }
        },
    }
}

/// A marketplace CC cannot load registers 0 hooks and 0 MCP for every plugin it
/// carries (probed 2.1.241), so a dangling one — moved, manifest-deleted, or
/// registered at a source diverged from the materialized pointer — is a Fail, not a
/// Warn: the plugin is dead until a heal re-points it.
#[cfg(feature = "claude")]
fn check_marketplace(cli: &ClaudeCli, plugin: &Plugin, source: &Source) -> DoctorCheck {
    let name = "marketplace registered";
    let marketplace = match find_marketplace(cli, &Scope::User, plugin.marketplace) {
        Ok(m) => m,
        Err(e) => return DoctorCheck { name, status: CheckStatus::Warn(format!("could not read `marketplace list --json`: {e}")) },
    };
    let expected = crate::current_pointer(plugin.name, ClaudeBackend.id());
    let expected = match expected {
        Ok(path) => path,
        Err(e) => return DoctorCheck { name, status: CheckStatus::Warn(format!("could not resolve the materialized pointer path: {e}")) },
    };
    match marketplace_health(marketplace.as_ref(), source, &expected) {
        MarketplaceHealth::Healthy => DoctorCheck { name, status: CheckStatus::Ok(format!("`{}` registered", plugin.marketplace)) },
        MarketplaceHealth::Absent => DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("marketplace `{}` is not registered; the plugin's hooks and MCP are not loaded", plugin.marketplace),
                fix: "run the host binary's `setup` (or `install`) subcommand".into(),
            },
        },
        MarketplaceHealth::Dangling => DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!(
                    "marketplace `{}` source is broken or registered elsewhere than the materialized tree; the plugin's hooks and MCP are not loaded",
                    plugin.marketplace
                ),
                fix: "run the host binary's `setup` (or `install`) subcommand".into(),
            },
        },
    }
}

#[cfg(feature = "claude")]
fn check_validate(plugin: &Plugin, source: &Source) -> DoctorCheck {
    let name = "manifest validates";
    let target = validate_target(plugin, source);
    let Some(target) = target else {
        return DoctorCheck {
            name,
            status: CheckStatus::Warn("no materialized tree to validate (embedded tree not yet materialized)".into()),
        };
    };
    let cli = match ClaudeCli::locate() {
        Ok(cli) => cli,
        Err(_) => return DoctorCheck { name, status: CheckStatus::Warn("skipped (claude not found)".into()) },
    };
    match cli.run_capturing(&["plugin", "validate", &target.display().to_string(), "--strict"], None) {
        Ok(out) if out.code == 0 => DoctorCheck { name, status: CheckStatus::Ok(format!("{} --strict clean", target.display())) },
        Ok(out) => DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("`validate --strict` failed:\n{}", String::from_utf8_lossy(&out.stderr).trim()),
                fix: "fix the reported manifest issues, then re-materialize with the host's `update`".into(),
            },
        },
        Err(e) => DoctorCheck { name, status: CheckStatus::Warn(format!("could not run validate: {e}")) },
    }
}

#[cfg(feature = "claude")]
fn validate_target(plugin: &Plugin, source: &Source) -> Option<PathBuf> {
    match source {
        // Both materialize the CC-scoped `current@claude`; that on-disk tree is the
        // validate target (doctor validates the tree CC would run).
        Source::Embedded | Source::Path(_) => {
            let current = data_root(plugin).ok()?.join(format!("current@{CLAUDE_CLIENT}"));
            current.exists().then_some(current)
        }
        Source::GitHub { .. } => None,
    }
}

#[cfg(feature = "claude")]
fn check_tree_hash(plugin: &Plugin, source: &Source) -> DoctorCheck {
    let name = "current tree matches embedded";
    if let Source::GitHub { .. } = source {
        return DoctorCheck { name, status: CheckStatus::Ok("github source; not applicable".into()) };
    }
    let current = match data_root(plugin) {
        Ok(root) => root.join(format!("current@{CLAUDE_CLIENT}")),
        Err(e) => return DoctorCheck { name, status: CheckStatus::Warn(format!("no data root: {e}")) },
    };
    if !current.exists() {
        return DoctorCheck { name, status: CheckStatus::Warn("nothing materialized yet".into()) };
    }
    // The on-disk `current@claude` is already token-substituted, so the baseline must
    // substitute too (a no-op for a token-free tree, so existing plugins are unchanged).
    // Embedded hashes the decompressed blob; Path hashes its on-disk source tree.
    let expected = match source {
        Source::Path(p) => dir_hash_for_client(p, CLAUDE_CLIENT),
        _ => tree_hash(plugin.blob(), CLAUDE_CLIENT),
    };
    match (dir_hash(&current), expected) {
        (Ok(on_disk), Ok(exp)) if on_disk == exp => DoctorCheck { name, status: CheckStatus::Ok("hashes match".into()) },
        (Ok(_), Ok(_)) => DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: "the `current` tree does not match its source tree (stale or corrupt pointer)".into(),
                fix: "re-run the host's `update` to re-materialize".into(),
            },
        },
        (a, b) => {
            let err = a.err().or(b.err()).map(|e| e.to_string()).unwrap_or_default();
            DoctorCheck { name, status: CheckStatus::Warn(format!("could not hash current tree: {err}")) }
        }
    }
}

/// Heuristic: collect bare command names the plugin's hook configs invoke and
/// check each resolves on PATH. Path/variable-prefixed commands are skipped
/// (they cannot be checked generically). Missing bare commands are the single
/// most common end-user breakage.
#[cfg(feature = "claude")]
fn check_hook_commands(plugin: &Plugin) -> DoctorCheck {
    let name = "hook commands on PATH";
    let mut commands = Vec::new();
    if let Err(e) = collect_hook_commands(plugin, &mut commands) {
        return DoctorCheck { name, status: CheckStatus::Warn(format!("could not read the embedded tree: {e}")) };
    }
    commands.sort();
    commands.dedup();

    let missing: Vec<String> = commands.into_iter().filter(|c| which::which(c).is_err()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok("all referenced bare commands resolve".into()) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("hook command(s) not on PATH: {}", missing.join(", ")),
                fix: "install the missing binaries into a PATH directory".into(),
            },
        }
    }
}

/// A host-AUTHORING check, like [`check_hook_commands`] above: the declared
/// status-line command must carry `${AGENTGEAR_CLIENT}` once two or more of the
/// host's agents can write a slot.
///
/// Each backend expands the token to its own id, and the host's own status-line
/// subcommand reads the stash of the client it was invoked for
/// ([`crate::statusline::user_original`]). Hardcode the client and every harness gets
/// the SAME literal command, so the host reads one backend's stash from all of them:
/// the user's own row is dropped, or another harness's stashed command runs inside
/// this one.
///
/// `None` — no check at all, not an `Ok` line — below two capable agents or with the
/// token present. There is nothing for the user to act on, and the slot's other
/// doctor check is opt-in by declaration the same way.
fn check_statusline_client(plugin: &Plugin) -> Option<DoctorCheck> {
    let command = plugin.statusline.as_ref().map(|decl| decl.command.as_str())?;
    if command.trim().is_empty() || command.contains(AGENTGEAR_CLIENT_TOKEN) {
        return None;
    }
    let capable: Vec<&str> = plugin
        .agents
        .iter()
        .copied()
        .filter(|id| crate::agents::backend_for(id).is_some_and(|backend| backend.capabilities().statusline))
        .collect();
    if capable.len() < 2 {
        return None;
    }
    Some(DoctorCheck {
        name: "status line client token",
        status: CheckStatus::Warn(format!(
            "the declared status-line command names no client, but {} each write their own status-line slot: \
             all of them get the same command, so the host reads one backend's displaced status line from every harness. \
             put {AGENTGEAR_CLIENT_TOKEN} in the command — each backend expands it to its own id",
            capable.join(", ")
        )),
    })
}

#[cfg(feature = "claude")]
fn collect_hook_commands(plugin: &Plugin, out: &mut Vec<String>) -> Result<()> {
    for (rel, bytes) in crate::materialize::blob_entries(plugin.blob())? {
        if is_hook_json(&rel)
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        {
            walk_commands(&value, out);
        }
    }
    Ok(())
}

#[cfg(feature = "claude")]
fn is_hook_json(rel: &str) -> bool {
    let rel = rel.replace('\\', "/");
    if !rel.ends_with(".json") {
        return false;
    }
    rel.rsplit('/').next() == Some("plugin.json") || rel.split('/').any(|c| c == "hooks")
}

#[cfg(feature = "claude")]
fn walk_commands(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, val) in map {
                if key == "command"
                    && let Value::String(cmd) = val
                    && let Some(bare) = bare_command(cmd)
                {
                    out.push(bare);
                }
                walk_commands(val, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| walk_commands(v, out)),
        _ => {}
    }
}

/// Return the first token of a hook command iff it is a plain executable name
/// (no path separator, no `$`-variable, no shell metacharacter that would make
/// the token something other than a PATH lookup).
#[cfg(feature = "claude")]
fn bare_command(command: &str) -> Option<String> {
    let token = command.split_whitespace().next()?;
    let looks_bare = !token.is_empty()
        && !token.contains('/')
        && !token.contains('\\')
        && !token.contains('$')
        && !token.starts_with('"')
        && token.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    looks_bare.then(|| token.to_string())
}

#[cfg(test)]
#[path = "../tests/unit/doctor.rs"]
mod doctor_tests;
