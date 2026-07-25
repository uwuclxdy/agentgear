//! doctor unit tests: the fan-out must degrade per agent, never abort the
//! report (a health command that discards its collected checks on the first
//! bad agent is useless exactly when it is needed).

use super::*;
use crate::host::Plugin;
use crate::statusline::StatusLineDecl;

fn plugin_with_agents(agents: &'static [&'static str]) -> Plugin {
    Plugin { name: "doctor-test", marketplace: "doctor-test", version: "0.0.0", agents, instructions: None, statusline: None, blob: &[] }
}

fn plugin_with_statusline(agents: &'static [&'static str], command: &str) -> Plugin {
    Plugin { statusline: Some(StatusLineDecl::new(command)), ..plugin_with_agents(agents) }
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

// `check_statusline_client`: a host-authoring warning. Two backends writing the SAME
// literal command means the host reads one backend's stash from every harness, so the
// user's own row is dropped or another harness's stashed command runs inside this one.
// The two-capable-backend cases need a second status-line backend compiled in.

const TOKEN_LESS: &str = "mytool statusline";
/// Only the two-capable-backend cases use it, and those need a second slot backend.
#[cfg(feature = "qwen-code")]
const WITH_TOKEN: &str = "mytool statusline --client ${AGENTGEAR_CLIENT}";

#[cfg(feature = "qwen-code")]
#[test]
fn a_client_blind_command_warns_once_two_backends_can_write_a_slot() {
    let plugin = plugin_with_statusline(&["claude", "qwen-code"], TOKEN_LESS);
    let check = check_statusline_client(&plugin).expect("two slot-capable agents plus no token must warn");
    match &check.status {
        CheckStatus::Warn(detail) => {
            assert!(detail.contains("claude") && detail.contains("qwen-code"), "the warn must name the agents: {detail}");
            assert!(detail.contains("${AGENTGEAR_CLIENT}"), "the warn must name the fix token: {detail}");
        }
        other => panic!("expected Warn, got {other:?}"),
    }
}

#[cfg(feature = "qwen-code")]
#[test]
fn the_client_token_clears_the_warning() {
    assert!(check_statusline_client(&plugin_with_statusline(&["claude", "qwen-code"], WITH_TOKEN)).is_none());
}

#[test]
fn one_capable_backend_never_warns() {
    // One slot to write means one stash to read: the client is unambiguous, so a
    // hardcoded one is correct and must not be flagged. `gemini` has no slot, and an
    // unresolvable id resolves to no backend at all — neither counts toward the two.
    assert!(check_statusline_client(&plugin_with_statusline(&["claude", "gemini"], TOKEN_LESS)).is_none());
    assert!(check_statusline_client(&plugin_with_statusline(&["claude", "not-a-backend"], TOKEN_LESS)).is_none());
    assert!(check_statusline_client(&plugin_with_statusline(&["claude"], TOKEN_LESS)).is_none());
}

#[test]
fn a_host_declaring_no_status_line_is_never_flagged() {
    assert!(check_statusline_client(&plugin_with_agents(&["claude", "qwen-code"])).is_none());
    // A blank declaration writes nothing, so it can carry no client either way.
    assert!(check_statusline_client(&plugin_with_statusline(&["claude", "qwen-code"], "   ")).is_none());
}
