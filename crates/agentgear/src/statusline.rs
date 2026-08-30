//! The runtime helpers a host's own status-line print subcommand composes with.
//!
//! agentgear no longer writes any harness's status-line slot: the automatic wiring
//! (the host declaration, the per-harness slot renderers, the stash-and-restore) is
//! retired. What remains is the render half — the host prints its own rows, then the
//! rows of whatever status line the user already had ([`user_original`] +
//! [`compose`]). A user who wants the host's line in their harness wires the print
//! subcommand into the harness config manually, and their own pre-existing line runs
//! natively beside it through that config — nothing is displaced, so nothing is
//! stashed anymore.
//!
//! The user's original row [`user_original`] returns comes from a legacy stamp
//! marker: only pre-retirement installs stashed one. Every marker written today
//! carries no stash, so the compose step contributes nothing on a fresh install and
//! the host's own rows render alone.

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

/// Presence-only marker that this process is already running inside a status-line
/// render. [`run_with_timeout`] both sets it on the child it spawns — so it reaches the
/// re-entered host binary through the shell — and refuses to spawn while it is already
/// set. Only presence is read; the value carries nothing beyond being non-empty.
const NESTED_RENDER_VAR: &str = "AGENTGEAR_STATUSLINE_NESTED";

/// One status-line command this crate (or a legacy stamp marker) knows about: the
/// shell command whose stdout is the rendered bar (line-oriented — each line is one
/// row), plus the harness's optional padding knob.
///
/// Deliberately open (no `#[non_exhaustive]`): a host constructs one directly, and
/// `..Default::default()` keeps working as fields are added.
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
    /// Read a declaration back out of a stashed raw value. `None` when the value
    /// carries no string `command` — the stash is kept verbatim in whatever shape
    /// the harness used, so a shape we cannot run is simply not run.
    pub(crate) fn from_value(value: &Value) -> Option<Self> {
        let command = value.get("command").and_then(Value::as_str)?;
        let padding = value.get("padding").and_then(Value::as_u64).and_then(|p| u8::try_from(p).ok());
        Some(Self { command: command.to_string(), padding })
    }
}

/// The status line this machine's user had before this crate's retired automatic
/// wiring displaced it, or `None` when nothing was stashed (or nothing is
/// installed). Only markers written by pre-retirement binaries carry a stash.
///
/// Scope resolution is project-then-user: with `cwd` set, the marker for that
/// project scope is consulted first and the user-scope marker is the fallback, so a
/// project install shadows the user one exactly as the harness's own precedence
/// does. `client` is the backend id the host was invoked for — a host reads it
/// straight off its own `--client` argument.
///
/// A stashed command that would re-enter the host binary from inside itself is not
/// filtered here: this reader answers what the marker HOLDS, and a host may ask for
/// reasons that never spawn anything, so a depth guard here would make it lie. The
/// re-entry is bounded at the spawn boundary instead ([`NESTED_RENDER_VAR`] in
/// [`run_with_timeout`]).
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
/// A user command that cannot be spawned, prints nothing, outlives the internal
/// timeout, or is reached from inside another render contributes nothing: a broken or
/// hung command of theirs must not blank the host's own bar.
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
    Ok(crate::stamp::read(plugin, scope, client)?.and_then(|m| m.statusline_original).as_ref().and_then(StatusLineDecl::from_value))
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
///
/// Also the spawn-depth guard, both halves: [`NESTED_RENDER_VAR`] is read on entry and
/// set on the child, so a render already nested inside another one returns `None` without
/// starting a process. Keeping the pair here means a later caller of this primitive
/// inherits the guard instead of having to remember it.
///
/// The sentinel goes on the child rather than on this process because the whole subtree
/// needs to see it (the shell hands it down to whatever it runs, host binary included),
/// and setting a process-global would need `unsafe`, which this crate forbids.
///
/// Not read in [`user_original`]: that reader is public and answers what the marker
/// holds, which a host may want for reasons that never spawn anything.
///
/// An empty value reads as absent. We only ever write `"1"`, so a blank one cannot be
/// ours — it means something else exported the name — and treating it as present would
/// silently drop the user's row on every render with nothing to observe from outside.
fn run_with_timeout(command: &str, session_json: &str, timeout: Duration) -> Option<String> {
    if std::env::var_os(NESTED_RENDER_VAR).is_some_and(|nested| !nested.is_empty()) {
        return None;
    }
    let mut child = shell_command(command)
        .env(NESTED_RENDER_VAR, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

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
