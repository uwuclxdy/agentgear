//! Minimal host binary exercising the full crate surface exactly as a real
//! consumer would: a `#[derive(PluginHost)]` struct + a one-line build.rs, with
//! `setup`/`self-heal`/`update`/`uninstall`/`doctor` subcommands.

use std::path::PathBuf;
use std::process::ExitCode;

use agentgear::{PluginHost, Scope, Source};

#[derive(PluginHost)]
#[plugin(name = "ez-fixture-plugin", agents = ["claude"])]
struct FixtureHost;

/// `--path <dir>` selects `Source::Path(dir)`; otherwise the embedded blob. Lets
/// the e2e drive the on-disk path source against the same fixture tree.
fn source_from_args() -> Source {
    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        if arg == "--path"
            && let Some(dir) = args.next()
        {
            return Source::Path(PathBuf::from(dir));
        }
    }
    Source::Embedded
}

fn main() -> ExitCode {
    let sub = std::env::args().nth(1).unwrap_or_default();
    match sub.as_str() {
        "setup" | "install" => report(FixtureHost::install(Scope::User, source_from_args())),
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
            eprintln!("usage: host_fixture <setup|self-heal|check-restart|update|uninstall|doctor> (got {other:?})");
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
