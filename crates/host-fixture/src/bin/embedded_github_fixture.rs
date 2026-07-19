//! An `embed = true`, `default_source = "github"` fixture host: the shape where
//! an explicit `install(Source::Embedded)` must stay embedded through
//! self_heal/uninstall instead of drifting to the github default (which skipped
//! the config-merge backend on every heal pass and orphaned its writes on
//! uninstall). Driven by the embedded-github-rehydrate hermetic test. No test
//! path ever reaches the network: every install here is embedded, and the only
//! github-capable backend (claude) is undetected in the hermetic env.

use std::process::ExitCode;

use agentgear::{AgentReport, PluginHost, Scope, Source};

#[derive(PluginHost)]
#[plugin(
    name = "ez-fixture-plugin",
    default_source = "github",
    github_repo = "uwuclxdy/agentgear",
    embed = true,
    agents = ["claude", "gemini"]
)]
struct EmbeddedGithubFixture;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        // Explicitly embedded, never `DEFAULT_SOURCE`: the point of this fixture
        // is an embedded install on a github-default host.
        Some("install") => print_report(EmbeddedGithubFixture::install_report(Scope::User, Source::Embedded)),
        Some("self-heal-report") => print_report(EmbeddedGithubFixture::self_heal_report()),
        Some("uninstall-report") => print_report(EmbeddedGithubFixture::uninstall_report(Scope::User)),
        other => {
            eprintln!("usage: embedded_github_fixture <install|self-heal-report|uninstall-report> (got {other:?})");
            ExitCode::from(2)
        }
    }
}

fn print_report(result: agentgear::Result<AgentReport>) -> ExitCode {
    match result {
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
