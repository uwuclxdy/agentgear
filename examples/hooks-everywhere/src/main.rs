//! A hooks-only agentgear host: this plugin's entire value is its hook wiring, so
//! this example shows the full `SessionStart` / `UserPromptSubmit` / `PreToolUse` /
//! `PostToolUse` lifecycle fanned out across every hook-capable harness agentgear
//! ships a backend for. No MCP server, no commands, no agents, no skills.
//!
//! Every harness here runs a hook synchronously on the request path (a prompt or a
//! tool call blocks on it), and several treat a hook's exit code as a verdict: a
//! non-zero `PreToolUse` exit can outright block the tool call. A hook body must
//! therefore stay fast, benign, and always exit 0 unless it deliberately means to
//! veto something — `guard` and `audit` below only append one log line and return.
//!
//! Subcommands:
//! - `setup [--agent <id>]... [--path <dir>]` — install; repeat `--agent` to narrow
//!   to specific backends (none = all `AGENTS`), `--path` installs a tree from disk
//!   instead of the embedded blob.
//! - `update`        — re-materialize + bump to the embedded version.
//! - `uninstall`     — remove our entries from every backend, keep the user's.
//! - `self-heal`     — the `SessionStart` hook target: repair a broken install,
//!   never resurrect a deliberate uninstall.
//! - `check-restart` — the `UserPromptSubmit` hook target: print the "restart
//!   Claude Code" notice when an update landed mid-session, else stay silent.
//! - `guard`         — the `PreToolUse` (matcher `Bash`) hook target.
//! - `audit`         — the `PostToolUse` hook target.
//! - `doctor`        — per-backend health report.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use agentgear::{PluginHost, Scope, Source};

#[derive(PluginHost)]
#[plugin(
    name = "hooks-everywhere",
    agents = [
        "claude", "codex", "gemini", "cursor", "cline", "devin", "qwen-code",
        "copilot-cli", "kimi", "goose", "crush", "droid", "augment",
        "antigravity-cli",
    ]
)]
struct HooksEverywhere;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("setup" | "install") => {
            let (source, agents) = parse_flags();
            let refs: Vec<&str> = agents.iter().map(String::as_str).collect();
            report(HooksEverywhere::install_into(Scope::User, source, &refs))
        }
        Some("update") => report(HooksEverywhere::update(Scope::User)),
        Some("uninstall") => report(HooksEverywhere::uninstall(Scope::User)),
        Some("self-heal") => report(HooksEverywhere::self_heal()),
        // Prints the restart notice as plain stdout (most harnesses feed non-JSON
        // hook stdout back as context) when an update is pending, else nothing.
        // Always exits 0 so a per-prompt hook never blocks the user.
        Some("check-restart") => {
            if let Some(message) = HooksEverywhere::restart_pending() {
                println!("{message}");
            }
            ExitCode::SUCCESS
        }
        Some("guard") => hook_sink("guard"),
        Some("audit") => hook_sink("audit"),
        Some("doctor") => match HooksEverywhere::doctor() {
            Ok(report) => {
                print!("{report}");
                if report.is_healthy() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        other => {
            eprintln!("usage: hooks-everywhere <setup|update|uninstall|self-heal|check-restart|guard|audit|doctor> (got {other:?})");
            ExitCode::from(2)
        }
    }
}

/// Parse `setup` flags: `--path <dir>` swaps the embedded blob for an on-disk tree;
/// each `--agent <id>` narrows the install to those backends (none = all `AGENTS`).
fn parse_flags() -> (Source, Vec<String>) {
    let mut source = Source::Embedded;
    let mut agents = Vec::new();
    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--path" => {
                if let Some(dir) = args.next() {
                    source = Source::Path(PathBuf::from(dir));
                }
            }
            "--agent" => {
                if let Some(id) = args.next() {
                    agents.push(id);
                }
            }
            _ => {}
        }
    }
    (source, agents)
}

fn report(result: agentgear::Result<agentgear::Outcome>) -> ExitCode {
    match result {
        Ok(outcome) => {
            println!("{outcome:?}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The `PreToolUse`/`PostToolUse` hook body: read whatever JSON payload the
/// harness pipes to stdin without parsing it (a demo hook has no business
/// rejecting a shape it doesn't recognize) and, if `$HOOKS_EVERYWHERE_LOG` is set,
/// append one line recording which hook fired and what it received. An unset env
/// or a write failure is a silent no-op: this trace is a convenience for the
/// example's tests, never a reason a real tool call should fail. Always exits 0.
fn hook_sink(name: &str) -> ExitCode {
    let mut payload = String::new();
    let _ = std::io::stdin().read_to_string(&mut payload);
    if let Ok(path) = std::env::var("HOOKS_EVERYWHERE_LOG") {
        let line = format!("{name}: {}\n", payload.trim().replace('\n', " "));
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(line.as_bytes());
        }
    }
    ExitCode::SUCCESS
}
