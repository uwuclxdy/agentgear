//! `doctor`: a structured, ordered health report. Every check carries a rendered
//! fix-hint, so a failing host binary tells the user what to do rather than
//! surfacing a stack trace (design §doctor).

use std::fmt;
use std::path::PathBuf;

use serde_json::Value;

use crate::cli::{ClaudeCli, MIN_CLAUDE_VERSION, parse_version};
use crate::error::Result;
use crate::host::{Plugin, Source, data_root};
use crate::materialize::{dir_hash, tree_hash};

const FLOOR: (u64, u64, u64) = (2, 1, 196);

#[derive(Debug, Clone)]
pub struct DoctorReport {
    checks: Vec<DoctorCheck>,
}

#[derive(Debug, Clone)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub status: CheckStatus,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CheckStatus {
    Ok(String),
    Warn(String),
    Fail { problem: String, fix: String },
}

impl DoctorReport {
    pub fn checks(&self) -> &[DoctorCheck] {
        &self.checks
    }

    /// True when no check failed (warnings are tolerated).
    pub fn is_healthy(&self) -> bool {
        !self.checks.iter().any(|c| matches!(c.status, CheckStatus::Fail { .. }))
    }

    pub(crate) fn from_error(err: crate::error::Error) -> Self {
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

pub(crate) fn doctor(plugin: &Plugin, source: Source) -> Result<DoctorReport> {
    let mut checks = Vec::new();

    checks.push(check_host_binary());

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
        checks.push(check_validate(plugin, source));
    }

    // These are pure local checks (hash the tree, resolve hook commands on PATH)
    // and stay useful even when `claude` is missing, so they run unconditionally.
    checks.push(check_tree_hash(plugin, source));
    checks.push(check_hook_commands(plugin));

    Ok(DoctorReport { checks })
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

fn check_claude_version(cli: &ClaudeCli) -> DoctorCheck {
    let name = "claude version";
    let raw = match cli.raw_version() {
        Ok(v) => v,
        Err(e) => return DoctorCheck { name, status: CheckStatus::Warn(format!("could not read `claude --version`: {e}")) },
    };
    match parse_version(&raw) {
        Some(v) if v < FLOOR => DoctorCheck {
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

fn check_validate(plugin: &Plugin, source: Source) -> DoctorCheck {
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

fn validate_target(plugin: &Plugin, source: Source) -> Option<PathBuf> {
    match source {
        Source::Embedded => {
            let current = data_root(plugin).ok()?.join("current");
            current.exists().then_some(current)
        }
        Source::GitHub { .. } => None,
    }
}

fn check_tree_hash(plugin: &Plugin, source: Source) -> DoctorCheck {
    let name = "current tree matches embedded";
    match source {
        Source::GitHub { .. } => DoctorCheck { name, status: CheckStatus::Ok("github source; not applicable".into()) },
        Source::Embedded => {
            let current = match data_root(plugin) {
                Ok(root) => root.join("current"),
                Err(e) => return DoctorCheck { name, status: CheckStatus::Warn(format!("no data root: {e}")) },
            };
            if !current.exists() {
                return DoctorCheck { name, status: CheckStatus::Warn("nothing materialized yet".into()) };
            }
            match dir_hash(&current) {
                Ok(on_disk) if on_disk == tree_hash(plugin.tree()) => DoctorCheck { name, status: CheckStatus::Ok("hashes match".into()) },
                Ok(_) => DoctorCheck {
                    name,
                    status: CheckStatus::Fail {
                        problem: "the `current` tree does not match the embedded tree (stale or corrupt pointer)".into(),
                        fix: "re-run the host's `update` to re-materialize".into(),
                    },
                },
                Err(e) => DoctorCheck { name, status: CheckStatus::Warn(format!("could not hash current tree: {e}")) },
            }
        }
    }
}

/// Heuristic: collect bare command names the plugin's hook configs invoke and
/// check each resolves on PATH. Path/variable-prefixed commands are skipped
/// (they cannot be checked generically). Missing bare commands are the single
/// most common end-user breakage.
fn check_hook_commands(plugin: &Plugin) -> DoctorCheck {
    let name = "hook commands on PATH";
    let mut commands = Vec::new();
    collect_hook_commands(plugin, &mut commands);
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

fn collect_hook_commands(plugin: &Plugin, out: &mut Vec<String>) {
    for file in json_files(plugin.tree()) {
        if let Ok(value) = serde_json::from_slice::<Value>(file) {
            walk_commands(&value, out);
        }
    }
}

fn json_files<'a>(tree: &'a include_dir::Dir<'a>) -> Vec<&'a [u8]> {
    fn walk<'a>(entries: &'a [include_dir::DirEntry<'a>], out: &mut Vec<&'a [u8]>) {
        for entry in entries {
            match entry {
                include_dir::DirEntry::Dir(d) => walk(d.entries(), out),
                include_dir::DirEntry::File(f) => {
                    let path = f.path();
                    let is_hook_json = path.extension().is_some_and(|e| e == "json")
                        && (path.file_name().is_some_and(|n| n == "plugin.json") || path.components().any(|c| c.as_os_str() == "hooks"));
                    if is_hook_json {
                        out.push(f.contents());
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(tree.entries(), &mut out);
    out
}

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
