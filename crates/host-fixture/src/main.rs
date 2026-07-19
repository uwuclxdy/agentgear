//! Minimal host binary exercising the full crate surface exactly as a real
//! consumer would: a `#[derive(PluginHost)]` struct + a one-line build.rs, with
//! `setup`/`self-heal`/`update`/`uninstall`/`doctor`/`mcp` subcommands. It lists
//! every backend so `setup --agent <id>` can target any one of them.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use agentgear::{PluginHost, Scope, Source};

#[derive(PluginHost)]
#[plugin(name = "ez-fixture-plugin", instructions_fn = fixture_instructions, agents = [
    "claude", "codex", "opencode", "gemini", "cursor", "cline", "devin",
    "qwen-code", "copilot-cli", "vscode-copilot", "jetbrains-copilot",
    "kimi", "kiro", "zed", "omp", "openclaw", "kilo",
    "antigravity", "antigravity-cli", "pi",
    "goose", "amp", "crush", "droid", "augment",
])]
struct FixtureHost;

/// Host-authored always-loaded guidance, exercising the `instructions` surface
/// (only opencode writes it today; other backends ignore `Plugin.instructions`).
fn fixture_instructions() -> Option<String> {
    Some("ez-fixture always-loaded guidance line.".to_string())
}

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
        // Same install, but printing the per-agent summary (`AgentReport`'s
        // Display) instead of the merged outcome; exercised by the fanout-report
        // hermetic test. Exit code follows `is_healthy` so a failed agent reds CI.
        "setup-report" => {
            let (source, agents) = parse_flags();
            let refs: Vec<&str> = agents.iter().map(String::as_str).collect();
            match FixtureHost::install_into_report(Scope::User, source, &refs) {
                Ok(report) => {
                    print!("{report}");
                    if report.is_healthy() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            }
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
            eprintln!("usage: host_fixture <setup|setup-report|self-heal|check-restart|mcp|update|uninstall|doctor> (got {other:?})");
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

/// A dependency-free newline-delimited JSON-RPC stdio MCP server. The loop reads
/// stdin until EOF and writes each reply; per-line dispatch lives in
/// `handle_request` so it is unit-testable without spawning the binary.
fn run_mcp_server() -> ExitCode {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(reply) = handle_request(line) {
            if writeln!(stdout, "{reply}").is_err() {
                break;
            }
            let _ = stdout.flush();
        }
    }
    ExitCode::SUCCESS
}

/// One MCP request line -> its JSON-RPC reply, or `None` for notifications. Ids
/// echo verbatim so a real client accepts the responses. The single `ping` tool
/// returns `pong`, giving the docker legs a deterministic round-trip to assert.
fn handle_request(line: &str) -> Option<String> {
    let id = json_id(line);
    match json_method(line).as_deref() {
        Some("initialize") => Some(format!(
            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"2024-11-05","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"ez-fixture","version":"0.1.0"}}}}}}"#
        )),
        Some("tools/list") => Some(format!(
            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[{{"name":"ping","description":"round-trip probe; returns pong","inputSchema":{{"type":"object","properties":{{}}}}}}]}}}}"#
        )),
        // `ping` is the only tool, so any `tools/call` gets the pong result.
        Some("tools/call") => {
            Some(format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"content":[{{"type":"text","text":"pong"}}],"isError":false}}}}"#))
        }
        // A request (has an id) we don't model still gets a well-formed reply;
        // a notification (no id) is fire-and-forget.
        Some(_) if id != "null" => Some(format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{}}}}"#)),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::handle_request;

    #[test]
    fn tools_list_advertises_ping() {
        let resp = handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap_or_default();
        assert!(resp.contains(r#""name":"ping""#), "tools/list did not advertise ping: {resp}");
    }

    #[test]
    fn tools_call_returns_pong() {
        let resp = handle_request(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ping"}}"#).unwrap_or_default();
        assert!(resp.contains(r#""text":"pong""#), "tools/call did not return pong: {resp}");
        assert!(resp.contains(r#""id":2"#), "id not echoed: {resp}");
    }

    #[test]
    fn initialize_advertises_tools_capability() {
        let resp = handle_request(r#"{"jsonrpc":"2.0","id":3,"method":"initialize"}"#).unwrap_or_default();
        assert!(resp.contains(r#""capabilities":{"tools":{}}"#), "initialize dropped tools cap: {resp}");
    }

    #[test]
    fn notification_gets_no_reply() {
        assert!(handle_request(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
    }
}
