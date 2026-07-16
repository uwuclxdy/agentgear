//! The "setup picker" pattern: a host that enumerates every backend its derive
//! declares and lets the caller target any subset, instead of installing (or
//! reporting on) all 25 at once.
//!
//! - `status`               — for every `MultiInstaller::AGENTS` id, resolve its
//!   backend via `agentgear::backend_for` and print detection + capabilities. This
//!   is the crate's public enumeration API, the reason this example exists.
//! - `setup [--agent <id>]...` — install; each `--agent` narrows to that backend,
//!   none = every detected one.
//! - `uninstall`  — remove our entries from every backend, keep the user's.
//! - `self-heal`  — the `SessionStart` hook target: repair a broken install.
//! - `doctor`     — per-backend health report.
//! - `mcp`        — this binary's own stdio MCP server.

use std::io::{BufRead, Write};
use std::process::ExitCode;

use agentgear::{PluginHost, Scope, Source};
use multi_installer::MultiInstaller;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("status") => {
            print_status();
            ExitCode::SUCCESS
        }
        Some("setup" | "install") => {
            let agents = parse_agents();
            let refs: Vec<&str> = agents.iter().map(String::as_str).collect();
            report(MultiInstaller::install_into(Scope::User, Source::Embedded, &refs))
        }
        Some("uninstall") => report(MultiInstaller::uninstall(Scope::User)),
        Some("self-heal") => report(MultiInstaller::self_heal()),
        Some("doctor") => match MultiInstaller::doctor() {
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
            eprintln!("usage: multi-installer <status|setup|uninstall|self-heal|doctor|mcp> (got {other:?})");
            ExitCode::from(2)
        }
    }
}

const ID_W: usize = 18;
const FLAG_W: usize = 8;

/// `status`: one row per `MultiInstaller::AGENTS` id — detected + what it can
/// host (mcp/hooks/plugins) + the scopes it supports.
fn print_status() {
    println!("{:<ID_W$} {:<FLAG_W$} {:<FLAG_W$} {:<FLAG_W$} {:<FLAG_W$} scopes", "id", "detected", "mcp", "hooks", "plugins");
    for id in MultiInstaller::AGENTS {
        let Some(backend) = agentgear::backend_for(id) else {
            println!("{id:<ID_W$} <backend not compiled in>");
            continue;
        };
        let caps = backend.capabilities();
        println!(
            "{:<ID_W$} {:<FLAG_W$} {:<FLAG_W$} {:<FLAG_W$} {:<FLAG_W$} {}",
            id,
            yn(backend.detect()),
            yn(caps.mcp),
            yn(caps.hooks),
            yn(caps.plugins),
            caps.scopes.join(", "),
        );
    }
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// Parse `setup` flags: each `--agent <id>` narrows the install to that backend
/// (none = every detected id in `MultiInstaller::AGENTS`).
fn parse_agents() -> Vec<String> {
    let mut agents = Vec::new();
    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        if arg == "--agent"
            && let Some(id) = args.next()
        {
            agents.push(id);
        }
    }
    agents
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
/// empty result, ignores notifications, and loops until stdin closes. Ids are
/// echoed verbatim so a real client accepts the responses.
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
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"2024-11-05","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"multi-installer","version":"0.1.0"}}}}}}"#
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
