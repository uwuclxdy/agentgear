//! Minimal host binary exercising the full crate surface exactly as a real
//! consumer would: a `#[derive(PluginHost)]` struct + a one-line build.rs, with
//! `setup`/`self-heal`/`update`/`uninstall`/`doctor`/`mcp` subcommands. It lists
//! every backend so `setup --agent <id>` can target any one of them.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use agentgear::{PluginHost, Scope, Source};

#[derive(PluginHost)]
#[plugin(name = "ez-fixture-plugin", agents = [
    "claude", "codex", "opencode", "gemini", "cursor", "cline", "devin",
    "qwen-code", "copilot-cli", "vscode-copilot", "jetbrains-copilot",
    "kimi", "kiro", "zed", "omp", "openclaw", "kilo",
    "antigravity", "antigravity-cli", "pi",
    "goose", "amp", "crush", "droid", "augment",
])]
struct FixtureHost;

/// Parse `setup`/`install` flags: `--path <dir>` selects `Source::Path`, else the
/// embedded blob; each `--agent <id>` narrows install to those backends (none = all).
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

fn main() -> ExitCode {
    let sub = std::env::args().nth(1).unwrap_or_default();
    match sub.as_str() {
        "setup" | "install" => {
            let (source, agents) = parse_flags();
            let refs: Vec<&str> = agents.iter().map(String::as_str).collect();
            report(FixtureHost::install_into(Scope::User, source, &refs))
        }
        "self-heal" => report(FixtureHost::self_heal()),
        // UserPromptSubmit hook entry: prints the restart-pending notice as plain
        // stdout (CC treats non-JSON stdout as context) when an update landed, else
        // silent. Always exits 0 so it never blocks the prompt.
        "check-restart" => {
            if let Some(message) = FixtureHost::restart_pending() {
                println!("{message}");
            }
            ExitCode::SUCCESS
        }
        // The fixture plugin's own mcp server, so a harness's `mcp list` can connect.
        "mcp" => run_mcp_server(),
        "update" => report(FixtureHost::update(Scope::User)),
        "uninstall" => report(FixtureHost::uninstall(Scope::User)),
        "doctor" => match FixtureHost::doctor() {
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
            eprintln!("usage: host_fixture <setup|self-heal|check-restart|mcp|update|uninstall|doctor> (got {other:?})");
            ExitCode::from(2)
        }
    }
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
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"2024-11-05","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"ez-fixture","version":"0.1.0"}}}}}}"#
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
