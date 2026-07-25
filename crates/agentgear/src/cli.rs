//! The single choke point for every supported-CLI invocation (`claude`,
//! `copilot`): locate the binary, scrub the session env a CC hook would otherwise
//! leak into the child, force non-interactive stdio, capture output. `claude`
//! parses `--json` tolerantly; `copilot` has no `--json` on any subcommand, so its
//! list commands parse copilot's TEXT output (see [`CopilotCli`]).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(feature = "claude")]
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// Minimum `claude` the crate supports: 2.1.196 is the first with per-entry
/// `validate` resolving sources against the manifest's own dir (design §concurrency).
#[cfg(feature = "claude")]
pub(crate) const MIN_CLAUDE_VERSION: &str = "2.1.196";
#[cfg(feature = "claude")]
pub(crate) const CLAUDE_FLOOR: (u64, u64, u64) = (2, 1, 196);

/// Raw result of one invocation, exit code included so callers that want to
/// inspect a nonzero exit (e.g. `validate`) can, rather than only erroring.
pub(crate) struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// The parent env keys a CC hook would otherwise leak into the child: the session
/// marker plus every `CLAUDE_CODE_*`. `CLAUDECODE` is always removed, set or not,
/// so the child can never inherit one from an outer session. `CLAUDE_CONFIG_DIR`
/// is deliberately absent from the list: an isolated test root must survive. Safe
/// for `copilot` too — it strips only CC session markers, never copilot's own env
/// (`COPILOT_HOME`/`COPILOT_GITHUB_TOKEN`/`GH_TOKEN`/`GITHUB_TOKEN`).
fn scrub_keys(vars: impl Iterator<Item = String>) -> Vec<String> {
    std::iter::once("CLAUDECODE".to_string()).chain(vars.filter(|key| key.starts_with("CLAUDE_CODE_"))).collect()
}

/// Build a non-interactive command: scrub the CC session env, null stdin (so no
/// prompt can hang), pipe out/err (so they are captured). Shared by every wrapper.
fn non_interactive(path: &Path, args: &[&str], cwd: Option<&Path>) -> Command {
    let mut cmd = Command::new(path);
    cmd.args(args);
    for key in scrub_keys(std::env::vars_os().filter_map(|(k, _)| k.into_string().ok())) {
        cmd.env_remove(key);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

/// Run and capture, regardless of exit code. `output()` drains both pipes
/// concurrently, so a large stderr never deadlocks a full stdout. `bin` labels the
/// spawn-failure context.
fn run_capturing(bin: &'static str, path: &Path, args: &[&str], cwd: Option<&Path>) -> Result<Output> {
    let out = non_interactive(path, args, cwd)
        .output()
        .map_err(|source| Error::Io { context: format!("spawning `{bin} {}`", args.join(" ")), source })?;
    Ok(Output { code: out.status.code().unwrap_or(-1), stdout: out.stdout, stderr: out.stderr })
}

/// Run and treat any nonzero exit as an error carrying the captured stderr.
fn run(bin: &'static str, path: &Path, args: &[&str], cwd: Option<&Path>) -> Result<Vec<u8>> {
    let out = run_capturing(bin, path, args, cwd)?;
    if out.code != 0 {
        return Err(Error::Cli {
            bin,
            args: args.join(" "),
            code: out.code,
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(out.stdout)
}

/// A located `claude` executable.
#[cfg(feature = "claude")]
pub(crate) struct ClaudeCli {
    path: PathBuf,
}

#[cfg(feature = "claude")]
impl ClaudeCli {
    pub fn locate() -> Result<Self> {
        which::which("claude").map(|path| Self { path }).map_err(|_| Error::ClaudeNotFound)
    }

    /// Run and capture, regardless of exit code.
    pub fn run_capturing(&self, args: &[&str], cwd: Option<&Path>) -> Result<Output> {
        run_capturing("claude", &self.path, args, cwd)
    }

    /// Run and treat any nonzero exit as an error carrying the captured stderr.
    pub fn run(&self, args: &[&str], cwd: Option<&Path>) -> Result<Vec<u8>> {
        run("claude", &self.path, args, cwd)
    }

    /// Run a `--json` command and parse tolerantly.
    pub fn run_json<T: DeserializeOwned>(&self, args: &[&str], cwd: Option<&Path>, what: &str) -> Result<T> {
        let stdout = self.run(args, cwd)?;
        serde_json::from_slice(&stdout).map_err(|source| Error::Json { what: what.to_string(), source })
    }

    pub fn raw_version(&self) -> Result<String> {
        let stdout = self.run(&["--version"], None)?;
        Ok(String::from_utf8_lossy(&stdout).trim().to_string())
    }

    /// Gate a mutating op on the version floor. A definitively-too-old `claude`
    /// hard-fails with an upgrade hint; an unparseable version warns and proceeds,
    /// matching the tolerant-json invariant (format varies across channels).
    pub fn ensure_min_version(&self) -> Result<()> {
        let raw = self.raw_version()?;
        match parse_version(&raw) {
            Some(v) if v < CLAUDE_FLOOR => Err(Error::ClaudeTooOld { found: raw, floor: MIN_CLAUDE_VERSION }),
            Some(_) => Ok(()),
            None => {
                eprintln!("agentgear: could not parse `claude --version` output {raw:?}; proceeding");
                Ok(())
            }
        }
    }
}

/// True when `installed` is a parseable version strictly older than `embedded`.
/// Unparseable or missing `installed` returns false, so we never churn (or
/// downgrade) on a version string we cannot read — the monotonic invariant.
pub(crate) fn version_lt(installed: Option<&str>, embedded: &str) -> bool {
    match (installed.and_then(parse_version), parse_version(embedded)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

/// Parse a leading `MAJOR.MINOR.PATCH` out of a version line such as
/// `"2.1.201 (Claude Code)"`. Trailing non-digits on the patch are tolerated.
pub(crate) fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    parse_version_token(s.split_whitespace().next()?)
}

/// Parse one whitespace-free token as `MAJOR.MINOR.PATCH`, tolerating trailing
/// non-digits on the patch (`1.2.3-beta`, `1.0.71.`).
fn parse_version_token(token: &str) -> Option<(u64, u64, u64)> {
    let mut parts = token.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch_digits: String = parts.next()?.chars().take_while(char::is_ascii_digit).collect();
    let patch = patch_digits.parse().ok()?;
    Some((major, minor, patch))
}

// --- copilot -----------------------------------------------------------------
//
// copilot has no `--json` on any subcommand and no `--scope`, so this wrapper adds
// small TEXT parsers for `plugin list` / `plugin marketplace list` in place of a
// `run_json`. Everything here is gated on the backend's own feature, so a
// claude-only (default) build carries none of it.

/// Minimum `copilot` for plugin management: the `plugin` lifecycle
/// (list/uninstall/update) did not exist before 1.0.71.
#[cfg(feature = "copilot-cli")]
pub(crate) const MIN_COPILOT_VERSION: &str = "1.0.71";
#[cfg(feature = "copilot-cli")]
const COPILOT_FLOOR: (u64, u64, u64) = (1, 0, 71);

/// A located `copilot` executable.
#[cfg(feature = "copilot-cli")]
pub(crate) struct CopilotCli {
    path: PathBuf,
}

/// One row of `copilot plugin list` (text — copilot has no `--json`), e.g.
/// `• <plugin>@<marketplace> (v0.1.0)`.
#[cfg(feature = "copilot-cli")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopilotPlugin {
    pub plugin: String,
    pub marketplace: String,
    pub version: Option<String>,
}

/// One row under the `Registered marketplaces:` section of `copilot plugin
/// marketplace list` (the built-in `Included with GitHub Copilot:` rows are never
/// ours). `path` is the local dir of a `(Local: <path>)` entry, else `None`.
#[cfg(feature = "copilot-cli")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopilotMarketplace {
    pub name: String,
    pub path: Option<String>,
}

#[cfg(feature = "copilot-cli")]
impl CopilotCli {
    pub fn locate() -> Result<Self> {
        which::which("copilot").map(|path| Self { path }).map_err(|_| Error::CopilotNotFound)
    }

    pub fn run(&self, args: &[&str], cwd: Option<&Path>) -> Result<Vec<u8>> {
        run("copilot", &self.path, args, cwd)
    }

    pub fn run_capturing(&self, args: &[&str], cwd: Option<&Path>) -> Result<Output> {
        run_capturing("copilot", &self.path, args, cwd)
    }

    pub fn raw_version(&self) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.run(&["--version"], None)?).trim().to_string())
    }

    /// Gate a plugin op on the 1.0.71 floor. `copilot --version` is
    /// `GitHub Copilot CLI 1.0.71.`, so the version token is not first; an
    /// unparseable version warns and proceeds (channels vary), matching claude.
    pub fn ensure_min_version(&self) -> Result<()> {
        let raw = self.raw_version()?;
        match copilot_meets_floor(&raw) {
            Some(true) => Ok(()),
            Some(false) => Err(Error::CopilotTooOld { found: raw }),
            None => {
                eprintln!("agentgear: could not parse `copilot --version` output {raw:?}; proceeding");
                Ok(())
            }
        }
    }

    /// Parsed `copilot plugin list` (no `--json`). `No plugins installed.` -> empty.
    pub fn plugin_list(&self, cwd: Option<&Path>) -> Result<Vec<CopilotPlugin>> {
        let stdout = self.run(&["plugin", "list"], cwd)?;
        Ok(parse_plugin_list(&String::from_utf8_lossy(&stdout)))
    }

    /// Parsed `copilot plugin marketplace list` (no `--json`), registered rows only.
    pub fn marketplace_list(&self, cwd: Option<&Path>) -> Result<Vec<CopilotMarketplace>> {
        let stdout = self.run(&["plugin", "marketplace", "list"], cwd)?;
        Ok(parse_marketplace_list(&String::from_utf8_lossy(&stdout)))
    }
}

/// `Some(true)` when a `copilot --version` string meets the 1.0.71 plugin-management
/// floor, `Some(false)` below it, `None` when unparseable.
#[cfg(feature = "copilot-cli")]
pub(crate) fn copilot_meets_floor(raw: &str) -> Option<bool> {
    parse_version_anywhere(raw).map(|v| v >= COPILOT_FLOOR)
}

/// Find the first whitespace token that parses as `MAJOR.MINOR.PATCH`. Unlike
/// [`parse_version`] (first token only), copilot's `--version` puts the version last
/// (`GitHub Copilot CLI 1.0.71.`).
#[cfg(feature = "copilot-cli")]
pub(crate) fn parse_version_anywhere(s: &str) -> Option<(u64, u64, u64)> {
    s.split_whitespace().find_map(parse_version_token)
}

/// Strip a leading list bullet (`•`/`◆`/`*`/`-`) and surrounding whitespace from a
/// copilot text row.
#[cfg(feature = "copilot-cli")]
fn strip_bullet(line: &str) -> &str {
    line.trim().trim_start_matches(['•', '◆', '*', '-']).trim()
}

/// Parse `copilot plugin list` text into rows. Each installed row is
/// `<plugin>@<marketplace> (v<version>)`; the `Installed plugins:` header and a
/// `No plugins installed.` line carry no `@` and are dropped. Defensive about the
/// bullet char and surrounding whitespace.
#[cfg(feature = "copilot-cli")]
pub(crate) fn parse_plugin_list(stdout: &str) -> Vec<CopilotPlugin> {
    stdout.lines().filter_map(parse_plugin_line).collect()
}

#[cfg(feature = "copilot-cli")]
fn parse_plugin_line(line: &str) -> Option<CopilotPlugin> {
    let line = strip_bullet(line);
    let (id, rest) = match line.split_once(' ') {
        Some((id, rest)) => (id, rest),
        None => (line, ""),
    };
    let (plugin, marketplace) = id.split_once('@')?;
    if plugin.is_empty() || marketplace.is_empty() {
        return None;
    }
    Some(CopilotPlugin { plugin: plugin.to_string(), marketplace: marketplace.to_string(), version: parse_paren_version(rest) })
}

/// Extract the version out of a `(v1.2.3)` suffix; `None` when absent/malformed.
#[cfg(feature = "copilot-cli")]
fn parse_paren_version(rest: &str) -> Option<String> {
    let start = rest.find("(v")? + 2;
    let end = rest[start..].find(')')? + start;
    let v = rest[start..end].trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// Parse `copilot plugin marketplace list` text, returning only rows under the
/// `Registered marketplaces:` header (the built-in `Included with GitHub Copilot:`
/// rows are never ours). Each row is `<name> (Local: <path>)` or `<name> (GitHub: <repo>)`.
#[cfg(feature = "copilot-cli")]
pub(crate) fn parse_marketplace_list(stdout: &str) -> Vec<CopilotMarketplace> {
    let mut in_registered = false;
    let mut out = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_section_header(trimmed) {
            in_registered = trimmed.starts_with("Registered marketplaces");
            continue;
        }
        if in_registered && let Some(m) = parse_marketplace_line(trimmed) {
            out.push(m);
        }
    }
    out
}

/// A section header ends with `:` and carries no bullet (`Included with GitHub
/// Copilot:`, `Registered marketplaces:`).
#[cfg(feature = "copilot-cli")]
fn is_section_header(line: &str) -> bool {
    line.ends_with(':') && !line.starts_with(['•', '◆', '*', '-'])
}

#[cfg(feature = "copilot-cli")]
fn parse_marketplace_line(line: &str) -> Option<CopilotMarketplace> {
    let line = strip_bullet(line);
    let (name, detail) = line.split_once(" (")?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let path = detail.strip_prefix("Local:").map(|p| p.trim_end_matches(')').trim().to_string());
    Some(CopilotMarketplace { name: name.to_string(), path })
}

// The unit bodies exercise the shared process/version helpers, which exist only in a
// plugin-native build.
#[cfg(test)]
#[path = "../tests/unit/cli.rs"]
mod cli_tests;
