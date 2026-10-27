//! A fixture host whose marketplace name carries a dot, so its plugin id is
//! `ez-fixture-plugin@ez.fixture`: the only shape that reaches the copilot-cli
//! backend's dotted-key paths (remove's by-hand `stale_key_note`, doctor's
//! `stale enabledPlugins key` warn). Driven by the dotted-key hermetic tests.
//! The `agents` list is the one dotted backend under test; nothing here touches
//! the network (the source is the embedded blob).

use std::process::ExitCode;

use agentgear::prelude::{PluginHost, Scope, Source};

#[derive(PluginHost)]
#[plugin(name = "ez-fixture-plugin", marketplace = "ez.fixture", agents = ["copilot-cli"])]
struct DottedFixture;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("setup") => report(DottedFixture::install(Scope::User, Source::Embedded)),
        Some("uninstall") => report(DottedFixture::uninstall(Scope::User)),
        Some("doctor") => match DottedFixture::doctor() {
            Ok(report) => {
                print!("{report}");
                if report.is_healthy() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
            }
            Err(e) => {
                eprintln!("error: {}", e.report());
                ExitCode::FAILURE
            }
        },
        other => {
            eprintln!("usage: dotted_fixture <setup|uninstall|doctor> (got {other:?})");
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
            eprintln!("error: {}", e.report());
            ExitCode::FAILURE
        }
    }
}
