//! doctor unit tests: the fan-out must degrade per agent, never abort the
//! report (a health command that discards its collected checks on the first
//! bad agent is useless exactly when it is needed).

use super::*;
use crate::host::Plugin;

fn plugin_with_agents(agents: &'static [&'static str]) -> Plugin {
    Plugin { name: "doctor-test", marketplace: "doctor-test", version: "0.0.0", agents, instructions: None, blob: &[] }
}

/// An unresolvable agent id becomes a failed CHECK inside an `Ok` report — the
/// host-binary check collected before it survives, and the report renders a fix.
#[test]
fn unresolvable_agent_is_a_failed_check_not_an_aborted_report() {
    let plugin = plugin_with_agents(&["definitely-not-a-backend"]);
    let report = doctor(&plugin, &Source::Embedded).expect("doctor must return a report, not abort");

    let checks = report.checks();
    assert_eq!(checks.len(), 2, "host-binary check + the failed resolve: {report}");
    assert_eq!(checks[1].name, "definitely-not-a-backend");
    match &checks[1].status {
        CheckStatus::Fail { problem, fix } => {
            assert!(problem.contains("no backend for agent `definitely-not-a-backend`"), "problem: {problem}");
            assert!(fix.contains("cargo feature"), "fix hint must name the feature: {fix}");
        }
        other => panic!("expected Fail, got {other:?}"),
    }
    assert!(!report.is_healthy());
}
