//! Typed errors. A recoverable "the environment is wrong, tell the user how to
//! fix it" failure (missing/old `claude`) is a distinct variant from a genuine
//! bug (io/json), so callers can render a fix-hint instead of a stack trace.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("`claude` CLI not found on PATH; install it with `npm install -g @anthropic-ai/claude-code`")]
    ClaudeNotFound,

    #[error("`claude` version {found} is below the required floor {floor}; upgrade with `npm install -g @anthropic-ai/claude-code`")]
    ClaudeTooOld { found: String, floor: &'static str },

    #[error("`claude {args}` failed with exit {code}:\n{stderr}")]
    Cli { args: String, code: i32, stderr: String },

    #[error("could not parse {what} as JSON: {source}")]
    Json {
        what: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    /// The embedded plugin tree is malformed (missing `plugin.json`, no author,
    /// etc.). This is an authoring bug in the host, surfaced with the fix.
    #[error("invalid plugin tree: {0}")]
    Tree(String),

    /// A mutating CLI call reported success but `list --json` does not reflect the
    /// expected end state. The CLI's own state is the reconcile target, so this is
    /// a genuine inconsistency, not a retryable bad-input error.
    #[error("post-op verification failed: {0}")]
    Verify(String),

    #[error("failed to acquire the shared lock at {path}: {source}")]
    Lock {
        path: PathBuf,
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
