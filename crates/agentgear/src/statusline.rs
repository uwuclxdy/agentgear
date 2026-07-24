//! The host-owned status-line surface: the [`StatusLineDecl`] a host declares
//! through `#[plugin(statusline_fn = ...)]`, plus the runtime helpers its own
//! status-line subcommand calls.
//!
//! agentgear never synthesizes a shell script. A backend writes the declared
//! command into the harness's own single status-line slot, and the host binary
//! that command names does the rendering — so the compose step (host rows first,
//! then whatever the user already had) happens inside the host, which is what
//! [`user_original`] and [`compose`] are for.
//!
//! # Two hosts on one machine
//!
//! The slot is strictly single-value and last-writer-wins. Two agentgear hosts
//! that both declare a status line therefore stack: B installs over A and stashes
//! A's command as "the user's original", so uninstalling A and then B restores A's
//! command rather than the user's true original. Accepted and unguarded — a guard
//! would need a cross-host registry agentgear deliberately does not own.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

use crate::error::Result;
use crate::host::{Plugin, Scope};

/// How long the user's own status command may run before its row is dropped. The
/// harness already bounds the whole status line and ours is now nested inside that,
/// so a command of theirs that hangs must not take the host's own rows down with it.
const USER_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

/// Ceiling on what a status command may print. The timeout bounds wall time, not
/// bytes: a command spraying stdout would otherwise have the reader allocating flat
/// out until it fires. Far above any real status line, so this is runaway protection
/// rather than a functional limit — output past it is simply cut.
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;

/// A host-declared status line: one shell command whose stdout is the rendered
/// bar (line-oriented — each line is one row), plus the harness's optional padding
/// knob.
///
/// Deliberately open (no `#[non_exhaustive]`): a host constructs one directly, and
/// `..Default::default()` keeps working as fields are added.
///
/// # Examples
///
/// ```
/// use agentgear::StatusLineDecl;
///
/// // `${AGENTGEAR_CLIENT}` expands to each backend's own client id on write.
/// let decl = StatusLineDecl::new("mytool statusline --client ${AGENTGEAR_CLIENT}").with_padding(0);
/// assert_eq!(decl.command, "mytool statusline --client ${AGENTGEAR_CLIENT}");
/// assert_eq!(decl.padding, Some(0));
///
/// let bare = StatusLineDecl::new("mytool statusline");
/// assert_eq!(bare.padding, None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusLineDecl {
    /// The shell command the harness runs. It receives the session JSON on stdin
    /// and its stdout is the bar.
    pub command: String,
    /// Harness padding, when the harness has one (Claude Code's `padding: 0`
    /// removes the leading column). `None` leaves the key unwritten.
    pub padding: Option<u8>,
}

impl StatusLineDecl {
    /// A declaration running `command`, with no padding override.
    pub fn new(command: impl Into<String>) -> Self {
        Self { command: command.into(), padding: None }
    }

    /// Set the harness padding.
    pub fn with_padding(mut self, padding: u8) -> Self {
        self.padding = Some(padding);
        self
    }

    /// Read a declaration back out of a stashed raw value. `None` when the value
    /// carries no string `command` — the stash is kept verbatim in whatever shape
    /// the harness used, so a shape we cannot run is simply not run.
    pub(crate) fn from_value(value: &Value) -> Option<Self> {
        let command = value.get("command").and_then(Value::as_str)?;
        let padding = value.get("padding").and_then(Value::as_u64).and_then(|p| u8::try_from(p).ok());
        Some(Self { command: command.to_string(), padding })
    }
}

/// The status line this machine's user had before `client`'s backend wrote the
/// host's own, or `None` when the slot was empty (or nothing is installed).
///
/// Scope resolution is project-then-user: with `cwd` set, the marker for that
/// project scope is consulted first and the user-scope marker is the fallback, so a
/// project install shadows the user one exactly as the harness's own precedence
/// does. `client` is the backend id the host was invoked for — a plugin's
/// `${AGENTGEAR_CLIENT}` token expands to it, so a host reads it straight off its
/// own `--client` argument.
///
/// A stash naming the host's own command reads as `None`: it can only have got there
/// through a bad write, and running it would re-enter this binary from inside itself.
///
/// Ceiling on the project lookup: `cwd` is matched against the project path the
/// install was scoped to, EXACTLY. A session started in a subdirectory of that root
/// keys a different marker and silently falls back to the user-scope stash, so a
/// project-scoped status line of the user's is dropped from the bar. The harness
/// itself walks up to find its project settings; this does not. Widening it means
/// walking `cwd`'s ancestors for a marker.
///
/// # Examples
///
/// ```
/// use agentgear::{Plugin, StatusLineDecl};
///
/// // `Plugin` only comes from `PluginHost::descriptor()`, so this reader is
/// // compile-checked against the live signature without building one.
/// fn original(plugin: &Plugin) -> agentgear::Result<Option<StatusLineDecl>> {
///     agentgear::statusline::user_original(plugin, "claude", None)
/// }
/// let _ = original as fn(&Plugin) -> agentgear::Result<Option<StatusLineDecl>>;
/// ```
pub fn user_original(plugin: &Plugin, client: &str, cwd: Option<&Path>) -> Result<Option<StatusLineDecl>> {
    for scope in lookup_scopes(cwd) {
        if let Some(decl) = stashed(plugin, &scope, client)? {
            return Ok(Some(decl));
        }
    }
    Ok(None)
}

/// Render the full status line: the host's own `rows` first, then the rows of
/// whatever status line the user already had (run with `session_json` on its
/// stdin, exactly as the harness would have run it). Concatenation is enough
/// because the surface is line-oriented — one output line is one row.
///
/// The project-or-user scope is taken from the session JSON's `cwd` (falling back
/// to `workspace.current_dir`), then resolved by [`user_original`].
///
/// A user command that cannot be spawned, prints nothing, or outlives the internal
/// timeout contributes nothing: a broken or hung command of theirs must not blank
/// the host's own bar.
///
/// # Examples
///
/// ```
/// use agentgear::Plugin;
///
/// fn render(plugin: &Plugin, session_json: &str) -> agentgear::Result<String> {
///     agentgear::statusline::compose(plugin, "claude", session_json, "mytool  main  ok")
/// }
/// let _ = render as fn(&Plugin, &str) -> agentgear::Result<String>;
/// ```
pub fn compose(plugin: &Plugin, client: &str, session_json: &str, rows: &str) -> Result<String> {
    let cwd = session_cwd(session_json);
    let ours = rows.trim_end_matches(['\n', '\r']);
    let Some(original) = user_original(plugin, client, cwd.as_deref())? else {
        return Ok(ours.to_string());
    };
    let Some(theirs) = run_status_command(&original.command, session_json) else {
        return Ok(ours.to_string());
    };
    Ok(match ours.is_empty() {
        true => theirs,
        false => format!("{ours}\n{theirs}"),
    })
}

/// The scopes [`user_original`] consults, in precedence order: a project scope
/// shadows the user one, matching the harness's own whole-value override.
fn lookup_scopes(cwd: Option<&Path>) -> Vec<Scope> {
    match cwd {
        Some(dir) => vec![Scope::Project { path: dir.to_path_buf() }, Scope::User],
        None => vec![Scope::User],
    }
}

fn stashed(plugin: &Plugin, scope: &Scope, client: &str) -> Result<Option<StatusLineDecl>> {
    let ours = own_command(plugin, client);
    Ok(crate::stamp::read(plugin, scope, client)?
        .and_then(|m| m.statusline_original)
        .as_ref()
        .and_then(StatusLineDecl::from_value)
        .filter(|decl| !is_own_command(decl, ours.as_deref())))
}

/// The command string a backend writes for `client`, `${AGENTGEAR_CLIENT}` expanded.
fn own_command(plugin: &Plugin, client: &str) -> Option<String> {
    plugin.statusline.as_ref().map(|decl| crate::components::expand_client(&decl.command, client))
}

/// Whether a stashed declaration names the host's OWN command.
///
/// Running one would re-enter the very binary the harness invoked, which reads the
/// same stash and spawns again — unbounded, and re-entered on every turn the harness
/// re-renders. A stash can only carry our command through a marker written by a
/// binary whose ownership test was wrong, or by another process; either way the
/// recovery is to drop the row, never to run it.
fn is_own_command(stashed: &StatusLineDecl, ours: Option<&str>) -> bool {
    ours == Some(stashed.command.as_str())
}

/// The session's working directory: Claude Code sends a top-level `cwd`, with
/// `workspace.current_dir` as the same value under the workspace block. Anything
/// unparseable resolves to user scope.
fn session_cwd(session_json: &str) -> Option<PathBuf> {
    let value: Value = serde_json::from_str(session_json).ok()?;
    let dir = value
        .get("cwd")
        .and_then(Value::as_str)
        .or_else(|| value.get("workspace").and_then(|w| w.get("current_dir")).and_then(Value::as_str))?;
    Some(PathBuf::from(dir))
}

fn run_status_command(command: &str, session_json: &str) -> Option<String> {
    run_with_timeout(command, session_json, USER_COMMAND_TIMEOUT)
}

/// Run the user's own status-line command through the platform shell with
/// `session_json` on stdin, returning its trailing-newline-trimmed stdout. `None` on
/// a spawn failure, empty output, or `timeout` elapsing (the child is killed and
/// reaped). The exit code is deliberately ignored — the harness itself renders
/// whatever a status command prints.
///
/// stdin and stdout are each drained on their own thread. Writing the whole payload
/// before reading deadlocks as soon as either side outgrows its pipe buffer, and the
/// calling thread has to stay free to enforce `timeout`.
fn run_with_timeout(command: &str, session_json: &str, timeout: Duration) -> Option<String> {
    let mut child = shell_command(command).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;

    if let Some(mut stdin) = child.stdin.take() {
        let payload = session_json.as_bytes().to_vec();
        std::thread::spawn(move || {
            // A command that never reads stdin closes it early; the broken pipe is
            // its choice, not a failure of ours. Dropping the handle sends EOF.
            let _ = stdin.write_all(&payload);
        });
    }
    let Some(mut stdout) = child.stdout.take() else {
        return reap(&mut child, None);
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let read = (&mut stdout).take(MAX_OUTPUT_BYTES).read_to_end(&mut buf);
        let _ = tx.send(read.map(|_| buf));
    });

    let collected = match rx.recv_timeout(timeout) {
        Ok(Ok(bytes)) => bytes,
        // Timed out, or the reader died: the child owns no output we can use.
        _ => return reap(&mut child, None),
    };
    let text = String::from_utf8_lossy(&collected).trim_end_matches(['\n', '\r']).to_string();
    reap(&mut child, (!text.is_empty()).then_some(text))
}

/// Kill (harmless once it has already exited) and reap the child so no zombie
/// outlives a host process that renders a status line every turn.
fn reap(child: &mut Child, out: Option<String>) -> Option<String> {
    let _ = child.kill();
    let _ = child.wait();
    out
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").arg(command);
    cmd
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd
}

#[cfg(test)]
#[path = "../tests/unit/statusline.rs"]
mod statusline_tests;
