//! A github-default fixture host (`default_source = "github"`, zero-embed): the
//! shape that used to hard-error mid-fan-out for every config-merge backend
//! (`entries_for` has no local tree for `Source::GitHub`). The fanout-report
//! hermetic test drives it to pin the decided behavior instead: a non-plugin-
//! native backend is a visible skip in every lifecycle report
//! (`setup`/`self-heal`/`update`/`uninstall`) and a Warn in `doctor`, while the
//! plugin-native pair (claude, copilot-cli) stays eligible. No test
//! path ever reaches the network: the plugin-native backends are undetected in
//! the hermetic env, and skipped agents never touch the source.

use std::process::ExitCode;

use agentgear::{PluginHost, Scope};

#[derive(PluginHost)]
#[plugin(
    name = "ez-fixture-plugin",
    default_source = "github",
    github_repo = "uwuclxdy/agentgear",
    embed = false,
    agents = ["claude", "gemini"]
)]
struct GithubFixture;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("setup-report") => match GithubFixture::install_report(Scope::User, GithubFixture::DEFAULT_SOURCE) {
            Ok(report) => {
                print!("{report}");
                if report.is_healthy() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        // Legacy merged path: a source-skipped backend must collapse to `Ok`,
        // never an error telling the user to use the source they are already on.
        Some("uninstall") => match GithubFixture::uninstall(Scope::User) {
            Ok(outcome) => {
                println!("{outcome:?}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("uninstall-report") => match GithubFixture::uninstall_report(Scope::User) {
            Ok(report) => {
                print!("{report}");
                if report.is_healthy() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("doctor") => match GithubFixture::doctor() {
            Ok(report) => {
                print!("{report}");
                if report.is_healthy() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("self-heal-report") => match GithubFixture::self_heal_report() {
            Ok(report) => {
                print!("{report}");
                if report.is_healthy() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        // Legacy merged path: a source-skipped backend must collapse to `Ok`,
        // never an error telling the user to stop using the source they're on.
        Some("self-heal") => match GithubFixture::self_heal() {
            Ok(outcome) => {
                println!("{outcome:?}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("update-report") => match GithubFixture::update_report(Scope::User) {
            Ok(report) => {
                print!("{report}");
                if report.is_healthy() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        // Legacy merged path: same source-skip-collapses-to-Ok contract as `self-heal`.
        Some("update") => match GithubFixture::update(Scope::User) {
            Ok(outcome) => {
                println!("{outcome:?}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        other => {
            eprintln!(
                "usage: github_fixture <setup-report|uninstall|uninstall-report|doctor|self-heal-report|self-heal|update-report|update> (got {other:?})"
            );
            ExitCode::from(2)
        }
    }
}
