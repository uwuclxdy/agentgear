//! The single choke point for every `claude plugin` invocation: locate the
//! binary, scrub the session env a CC hook would otherwise leak into the child,
//! force non-interactive stdio, capture output, and parse `--json` tolerantly.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// Minimum `claude` the crate supports: 2.1.196 is the first with per-entry
/// `validate` resolving sources against the manifest's own dir (design §concurrency).
pub(crate) const MIN_CLAUDE_VERSION: &str = "2.1.196";
const FLOOR: (u64, u64, u64) = (2, 1, 196);

/// A located `claude` executable.
pub(crate) struct ClaudeCli {
    path: PathBuf,
}

/// Raw result of one invocation, exit code included so callers that want to
/// inspect a nonzero exit (e.g. `validate`) can, rather than only erroring.
pub(crate) struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ClaudeCli {
    pub fn locate() -> Result<Self> {
        which::which("claude").map(|path| Self { path }).map_err(|_| Error::ClaudeNotFound)
    }

    /// Build a `claude <args>` command with the session env scrubbed and stdio
    /// wired for a non-interactive context (null stdin => no prompt can hang;
    /// piped out/err => captured). `CLAUDE_CONFIG_DIR` is deliberately preserved
    /// so an isolated test root survives the scrub.
    fn command(&self, args: &[&str], cwd: Option<&Path>) -> Command {
        let mut cmd = Command::new(&self.path);
        cmd.args(args);
        cmd.env_remove("CLAUDECODE");
        for (key, _) in std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v))) {
            if key.starts_with("CLAUDE_CODE_") {
                cmd.env_remove(key);
            }
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd
    }

    /// Run and capture, regardless of exit code. `output()` drains both pipes
    /// concurrently, so a large stderr never deadlocks a full stdout.
    pub fn run_capturing(&self, args: &[&str], cwd: Option<&Path>) -> Result<Output> {
        let out = self.command(args, cwd).output().io_ctx_run(args)?;
        Ok(Output { code: out.status.code().unwrap_or(-1), stdout: out.stdout, stderr: out.stderr })
    }

    /// Run and treat any nonzero exit as an error carrying the captured stderr.
    pub fn run(&self, args: &[&str], cwd: Option<&Path>) -> Result<Vec<u8>> {
        let out = self.run_capturing(args, cwd)?;
        if out.code != 0 {
            return Err(Error::Cli {
                args: args.join(" "),
                code: out.code,
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(out.stdout)
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
            Some(v) if v < FLOOR => Err(Error::ClaudeTooOld { found: raw, floor: MIN_CLAUDE_VERSION }),
            Some(_) => Ok(()),
            None => {
                eprintln!("ez-agent-plugin: could not parse `claude --version` output {raw:?}; proceeding");
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
    let token = s.split_whitespace().next()?;
    let mut parts = token.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch_digits: String = parts.next()?.chars().take_while(char::is_ascii_digit).collect();
    let patch = patch_digits.parse().ok()?;
    Some((major, minor, patch))
}

trait RunContext<T> {
    fn io_ctx_run(self, args: &[&str]) -> Result<T>;
}

impl<T> RunContext<T> for std::io::Result<T> {
    fn io_ctx_run(self, args: &[&str]) -> Result<T> {
        self.map_err(|source| Error::Io { context: format!("spawning `claude {}`", args.join(" ")), source })
    }
}

#[cfg(test)]
#[path = "../tests/unit/cli.rs"]
mod cli_tests;
