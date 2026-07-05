//! Minimal host binary exercising the full crate surface exactly as a real
//! consumer would: a `#[derive(PluginHost)]` struct + a one-line build.rs, with
//! `setup`/`self-heal`/`update`/`uninstall`/`doctor` subcommands.

use std::path::PathBuf;
use std::process::ExitCode;

use ez_agent_plugin::{PluginHost, Scope, Source};

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
            eprintln!("usage: host_fixture <setup|self-heal|update|uninstall|doctor> (got {other:?})");
            ExitCode::from(2)
        }
    }
}

fn report(result: ez_agent_plugin::Result<ez_agent_plugin::Outcome>) -> ExitCode {
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
