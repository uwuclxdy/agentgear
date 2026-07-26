//! Typed errors. A recoverable "the environment is wrong, tell the user how to
//! fix it" failure (missing/old `claude`) is a distinct variant from a genuine
//! bug (io/json), so callers can render a fix-hint instead of a stack trace.

use std::path::PathBuf;

/// The crate's result alias over [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Everything a lifecycle call can fail with. Environment problems the user can fix
/// (missing or old `claude`/`copilot`) are distinct variants from genuine bugs
/// (io/json), so a caller renders a fix-hint rather than a stack trace.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The `claude` CLI is not on `PATH`.
    #[error("`claude` CLI not found on PATH; install it with `npm install -g @anthropic-ai/claude-code`")]
    ClaudeNotFound,

    /// The located `claude` is older than the supported floor.
    #[error("`claude` version {found} is below the required floor {floor}; upgrade with `npm install -g @anthropic-ai/claude-code`")]
    ClaudeTooOld {
        /// The version `claude --version` reported.
        found: String,
        /// The minimum version the crate requires.
        floor: &'static str,
    },

    /// The `copilot` CLI is not on `PATH`.
    #[error("`copilot` CLI not found on PATH; install it with `npm install -g @github/copilot`")]
    CopilotNotFound,

    /// The located `copilot` predates plugin-management support.
    #[error("copilot >= 1.0.71 required for plugin management (found {found}); run `copilot update`")]
    CopilotTooOld {
        /// The version `copilot --version` reported.
        found: String,
    },

    /// A config-dir env override (`CLAUDE_CONFIG_DIR`, `COPILOT_HOME`) was set to the
    /// empty string. The CLI it targets resolves that literally, joining its config
    /// paths onto the empty string instead of falling back to its default, so this is
    /// rejected outright rather than silently treated as unset.
    #[error(
        "`{var}` is set to an empty string; unset it or point it at a real directory. \
         An empty override resolves against the current directory, so the write would \
         land where the CLI never reads it."
    )]
    EmptyConfigDirOverride {
        /// The environment variable that was set to the empty string.
        var: &'static str,
    },

    /// A CLI call exited non-zero.
    #[error("`{bin} {args}` failed with exit {code}:\n{stderr}")]
    #[non_exhaustive]
    Cli {
        /// The invoked binary.
        bin: &'static str,
        /// Its argument line.
        args: String,
        /// Its exit code.
        code: i32,
        /// Captured stderr.
        stderr: String,
    },

    /// A JSON document did not parse.
    #[error("could not parse {what} as JSON: {source}")]
    Json {
        /// What was being parsed (for the message).
        what: String,
        /// The underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// A filesystem operation failed.
    #[error("{context}: {source}")]
    Io {
        /// The operation being attempted.
        context: String,
        /// The underlying io error.
        #[source]
        source: std::io::Error,
    },

    /// The embedded plugin tree is malformed (missing `plugin.json`, no author,
    /// etc.). This is an authoring bug in the host, surfaced with the fix.
    #[error("invalid plugin tree: {0}")]
    Tree(String),

    /// A harness config file exists but does not parse; a read-modify-write
    /// refuses to clobber it rather than risk destroying the user's config.
    #[error("could not parse config {path}: {detail}")]
    Config {
        /// The config file that failed to parse.
        path: String,
        /// The parse failure, rendered for the user.
        detail: String,
    },

    /// An out-of-crate [`AgentBackend`](crate::AgentBackend) failure. The other
    /// variants all carry in-crate semantics (CLI orchestration, tree parsing,
    /// config merging), so an external backend maps its own failures into this
    /// neutral shape instead of borrowing one of those meanings.
    #[error("{agent}: {detail}")]
    Backend {
        /// The backend's [`id`](crate::AgentBackend::id).
        agent: String,
        /// What went wrong, rendered for the user.
        detail: String,
    },

    /// A mutating CLI call reported success but `list --json` does not reflect the
    /// expected end state. The CLI's own state is the reconcile target, so this is
    /// a genuine inconsistency, not a retryable bad-input error.
    #[error("post-op verification failed: {0}")]
    Verify(String),

    /// The shared cross-process lock could not be acquired.
    #[error("failed to acquire the shared lock at {path}: {source}")]
    Lock {
        /// The lock file.
        path: PathBuf,
        /// The underlying io error.
        #[source]
        source: std::io::Error,
    },
}

/// Attach a human context string to an `io::Result`, converting it into our
/// error. Keeps call sites to `.io_ctx(|| format!(...))?` without a match.
pub(crate) trait IoContext<T> {
    fn io_ctx(self, f: impl FnOnce() -> String) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn io_ctx(self, f: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|source| Error::Io { context: f(), source })
    }
}
