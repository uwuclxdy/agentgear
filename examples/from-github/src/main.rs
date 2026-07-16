//! A zero-embed agentgear host driving the plugin lifecycle from a GitHub
//! marketplace: the "install without shipping the tree in the binary" surface.
//!
//! - `setup`      — install from `FromGithub::DEFAULT_SOURCE` (the GitHub tag). A
//!   zero-embed host has no baked blob, so `Source::Embedded` would error at
//!   materialize; `setup --embedded` forces that wrong source on purpose to show the
//!   failure this example teaches.
//! - `update`     — re-point/refresh to the tracked GitHub ref.
//! - `uninstall`  — remove our entry, keep the user's.
//! - `self-heal`  — the `SessionStart` hook target: repair a broken install off the
//!   GitHub source, never resurrect a deliberate uninstall.
//! - `doctor`     — health report.

use std::process::ExitCode;

use agentgear::{PluginHost, Scope, Source};
use from_github::FromGithub;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("setup" | "install") => {
            // The correct call for a zero-embed host keys on the GitHub tag. Passing
            // `Source::Embedded` here (via `--embedded`) errors on the empty blob:
            // there is nothing baked in to materialize.
            let source = if wants_embedded() { Source::Embedded } else { FromGithub::DEFAULT_SOURCE };
            report(FromGithub::install(Scope::User, source))
        }
        Some("update") => report(FromGithub::update(Scope::User)),
        Some("uninstall") => report(FromGithub::uninstall(Scope::User)),
        Some("self-heal") => report(FromGithub::self_heal()),
        Some("doctor") => match FromGithub::doctor() {
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
            eprintln!("usage: from-github <setup [--embedded]|update|uninstall|self-heal|doctor> (got {other:?})");
            ExitCode::from(2)
        }
    }
}

/// `setup --embedded` deliberately requests the (absent) baked blob to demonstrate
/// the zero-embed failure; without it `setup` uses the GitHub `DEFAULT_SOURCE`.
fn wants_embedded() -> bool {
    std::env::args().skip(2).any(|a| a == "--embedded")
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
