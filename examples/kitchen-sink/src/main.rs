//! A full-surface agentgear host, meant to be copied and trimmed. It ships every
//! plugin component type (MCP server, hooks, a command, a subagent, a skill) and
//! wires the whole lifecycle across seven harnesses (`claude` + six non-CC backends):
//!
//! - `setup [--agent <id>]... [--path <dir>]` (alias `install`) — install; `--agent`
//!   narrows to one backend (what a per-harness installer or test drives), `--path`
//!   installs a tree from disk instead of the embedded blob.
//! - `update`     — re-materialize + bump to the embedded version.
//! - `uninstall`  — remove our entries from every backend, keep the user's.
//! - `self-heal`  — the `SessionStart` hook target: repair a broken install, never
//!   resurrect a deliberate uninstall.
//! - `check-restart` — the `UserPromptSubmit` hook target: print the "restart Claude
//!   Code" notice when an update landed mid-session, else stay silent.
//! - `doctor`     — per-backend health report.
//! - `mcp`        — this binary's own stdio MCP server (the tree points its MCP entry
//!   back at `kitchen-sink mcp`, so every harness gets a live server).

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use agentgear::{PluginHost, Scope, Source};

#[derive(PluginHost)]
#[plugin(name = "kitchen-sink", agents = ["claude", "codex", "opencode", "gemini", "cursor", "crush", "goose"])]
struct KitchenSink;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("setup" | "install") => {
            let (source, agents) = parse_flags();
            let refs: Vec<&str> = agents.iter().map(String::as_str).collect();
            report(KitchenSink::install_into(Scope::User, source, &refs))
        }
        Some("update") => report(KitchenSink::update(Scope::User)),
        Some("uninstall") => report(KitchenSink::uninstall(Scope::User)),
        Some("self-heal") => report(KitchenSink::self_heal()),
        // Prints the restart notice as plain stdout (Claude Code feeds non-JSON hook
        // stdout back as context) when an update is pending, else nothing. Always
        // exits 0 so a per-prompt hook never blocks the user.
        Some("check-restart") => {
            if let Some(message) = KitchenSink::restart_pending() {
                println!("{message}");
            }
            ExitCode::SUCCESS
        }
        Some("doctor") => match KitchenSink::doctor() {
            Ok(report) => {
                print!("{report}");
                if report.is_healthy() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("mcp") => run_mcp_server(),
        other => {
            eprintln!("usage: kitchen-sink <setup|update|uninstall|self-heal|check-restart|doctor|mcp> (got {other:?})");
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

/// A dependency-free newline-delimited JSON-RPC stdio MCP server: answers
/// `initialize` + `tools/list` (no tools), replies to any other request with an
/// empty result, ignores notifications, and loops until stdin closes. Ids are echoed
/// verbatim so a real client accepts the responses. Real hosts would use an MCP SDK;
/// this hand-rolled responder keeps the example free of extra dependencies.
fn run_mcp_server() -> ExitCode {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let id = json_id(line);
        let reply = match json_method(line).as_deref() {
            Some("initialize") => Some(format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"2024-11-05","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"kitchen-sink","version":"0.1.0"}}}}}}"#
            )),
            Some("tools/list") => Some(format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[]}}}}"#)),
            // A request (has an id) we don't model still gets a well-formed reply;
            // a notification (no id) is fire-and-forget.
            Some(_) if id != "null" => Some(format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{}}}}"#)),
            _ => None,
        };
        if let Some(reply) = reply {
            if writeln!(stdout, "{reply}").is_err() {
                break;
            }
            let _ = stdout.flush();
        }
    }
    ExitCode::SUCCESS
}

/// The raw JSON-RPC `id` token (a number, or a `"quoted"` string kept with its
/// quotes), echoed back verbatim; `"null"` when the message carries no id.
fn json_id(line: &str) -> String {
    let Some(rest) = value_after(line, "\"id\"") else { return "null".to_string() };
    if let Some(after_quote) = rest.strip_prefix('"') {
        let end = after_quote.find('"').map(|e| e + 2).unwrap_or(rest.len());
        rest[..end].to_string()
    } else {
        let end = rest.find([',', '}', ' ', '\t']).unwrap_or(rest.len());
        rest[..end].trim().to_string()
    }
}

fn json_method(line: &str) -> Option<String> {
    let rest = value_after(line, "\"method\"")?.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The slice right after `"<key>":` (whitespace trimmed), or `None` if absent.
fn value_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let i = line.find(key)? + key.len();
    Some(line.get(i..)?.trim_start().strip_prefix(':')?.trim_start())
}
