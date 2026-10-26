//! Which `SessionStart` event fired, and whether it may retire the
//! restart-pending notice. `startup` and `resume` began by loading the plugin
//! tree from disk, so a healthy heal under them runs in a session holding the
//! current tree. `/compact` and `/clear` fire inside the session that already
//! loaded the plugin — CC memoizes plugin hook registration per session (probed
//! 2.1.310) and does not re-read the tree there — so a heal under those still
//! runs in the session holding the OLD tree and must leave the notice standing.
//! A fork is not proof either way and is documented on [`SessionSource::Fork`].

use std::io::{IsTerminal, Read};

use serde::Deserialize;

/// The largest hook payload read off stdin. A `SessionStart` payload is a few
/// hundred bytes; the bound keeps a caller-owned pipe that never closes from
/// hanging a heal, and a payload cut at the cap fails to parse as `Unknown`.
const MAX_HOOK_PAYLOAD: u64 = 64 * 1024;

/// The `source` field of a `SessionStart` hook payload, mapped to the one
/// decision it drives: may a heal fired by this event retire the restart notice?
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionSource {
    /// A fresh session that loaded the plugin tree from disk.
    Startup,
    /// A resumed session that loaded the plugin tree from disk.
    Resume,
    /// `/clear` inside the running session: the plugin was not re-read, so the
    /// session still holds the tree it loaded at startup.
    Clear,
    /// `/compact` inside the running session: the plugin was not re-read.
    Compact,
    /// A forked session. It never counts as a fresh load, so no reconcile or
    /// healthy row retires a notice on it (the clean-uninstall clear is the one
    /// exception, see [`SessionSource::loaded_plugin_contents`]): an in-process
    /// fork inherits the parent session's memoized hook registration, the payload
    /// does not distinguish it from a `--fork-session` process that reloads the
    /// tree, and retiring a true notice strands the session on the old tree.
    /// The notice outliving a fork costs one session's nag instead.
    Fork,
    /// No recognizable source: a manual run, a non-hook caller, or a payload
    /// this crate does not understand. Never retires the notice.
    Unknown,
}

impl SessionSource {
    /// Map the hook payload's exact `source` string. Only the five lowercase
    /// literals CC documents are recognized; any other spelling (a cased
    /// variant, a future event) is [`SessionSource::Unknown`], so an
    /// unrecognized event never counts as a fresh load.
    pub fn from_hook_source(source: &str) -> Self {
        match source {
            "startup" => Self::Startup,
            "resume" => Self::Resume,
            "clear" => Self::Clear,
            "compact" => Self::Compact,
            "fork" => Self::Fork,
            _ => Self::Unknown,
        }
    }

    /// Read the top-level `source` string out of a full `SessionStart` hook JSON
    /// payload. An absent, malformed, or non-string `source` is
    /// [`SessionSource::Unknown`]: a run that cannot prove it loaded the tree
    /// must never retire the notice.
    pub fn from_hook_payload(payload: &str) -> Self {
        let Ok(hook) = serde_json::from_str::<HookPayload>(payload) else {
            return Self::Unknown;
        };
        hook.source.as_deref().map_or(Self::Unknown, Self::from_hook_source)
    }

    /// The source of the run whose stdin carries the hook payload. A terminal
    /// stdin means a manual run, not a hook, and reads [`SessionSource::Unknown`]
    /// without blocking; an stdin read error reads the same.
    pub fn from_stdin() -> Self {
        let stdin = std::io::stdin();
        if stdin.is_terminal() {
            return Self::Unknown;
        }
        let mut payload = String::new();
        if stdin.take(MAX_HOOK_PAYLOAD).read_to_string(&mut payload).is_err() {
            return Self::Unknown;
        }
        Self::from_hook_payload(&payload)
    }

    /// Whether this event began by loading plugin contents from disk, the only
    /// proof that the running session holds the tree a heal reconciles against.
    /// Only [`Startup`](Self::Startup) and [`Resume`](Self::Resume) qualify; see
    /// [`Fork`](Self::Fork) for why a fork is excluded. This gates the healthy row
    /// and the no-change reconcile; the clean-uninstall heal clears a standing
    /// notice under any event, since the plugin is gone and the notice is moot.
    pub fn loaded_plugin_contents(self) -> bool {
        matches!(self, Self::Startup | Self::Resume)
    }
}

/// The one field this crate reads from a hook payload; unknown fields are
/// ignored (serde's default), so an additive CC payload change still parses.
#[derive(Deserialize)]
struct HookPayload {
    source: Option<String>,
}

#[cfg(test)]
#[path = "../tests/unit/session.rs"]
mod session_tests;
