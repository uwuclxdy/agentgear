//! A three-command binary that installs its embedded Claude Code plugin: the
//! "60 seconds to a working plugin" surface. `setup` replaces the user typing
//! `/plugin marketplace add` + `/plugin install`; `uninstall` reverses it; `doctor`
//! reports whether the install is healthy.

use std::process::ExitCode;

use agentgear::{PluginHost, Scope, Source};
use hello_mcp::HelloMcp;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        // `Source::Embedded` materializes the baked-in tree; user scope installs it
        // into the current account (the only scope a binary-driven install supports).
        Some("setup") => report(HelloMcp::install(Scope::User, Source::Embedded)),
        Some("uninstall") => report(HelloMcp::uninstall(Scope::User)),
        Some("doctor") => match HelloMcp::doctor() {
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
            eprintln!("usage: hello-mcp <setup|uninstall|doctor> (got {other:?})");
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
