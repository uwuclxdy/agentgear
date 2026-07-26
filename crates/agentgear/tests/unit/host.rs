//! host unit tests: `Scope`'s pure key derivation. Unix-only (needs a real
//! symlink); matches this crate's existing precedent for platform-gating
//! filesystem-symlink assertions (e.g. `cline`'s executable-bit check).

use super::*;

/// The project-scope stamp key is per-project-PATH, so a project reached via a
/// symlink and via its realpath must key the SAME marker — an unresolved raw
/// path would double-install. Built from an explicit symlink (not an assumption
/// that `TMPDIR` itself is symlinked), per the repo's own realpath-comparison
/// learning.
#[cfg(unix)]
#[test]
fn project_scope_key_is_stable_through_a_symlink() {
    let root = crate::scratch::path("agentgear-scope-key");
    let real = root.join("real-project");
    std::fs::create_dir_all(&real).expect("create real project dir");
    let link = root.join("link-to-project");
    std::os::unix::fs::symlink(&real, &link).expect("create symlink to project dir");

    let via_real = Scope::Project { path: real.clone() }.key();
    let via_link = Scope::Project { path: link.clone() }.key();

    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(via_real, via_link, "a symlinked project path must key the same stamp marker as its realpath");
}

/// Pin representative surface flags so a wrong `capabilities()` reds a unit test,
/// not only the multi-installer status output: crush translates commands but has no
/// subagent surface, openclaw translates skills, codex translates neither skills nor
/// plugins. Gated on all three features (lit under `--all-features`, the gate).
#[cfg(all(feature = "crush", feature = "openclaw", feature = "codex"))]
#[test]
fn representative_backend_capability_flags() {
    use crate::agents::AgentBackend;

    let crush = crate::agents::crush::CrushBackend.capabilities();
    assert!(crush.commands, "crush translates commands");
    assert!(!crush.agents, "crush has no file-writable subagent surface (#1807)");
    assert!(crush.skills, "crush translates skills");

    assert!(crate::agents::openclaw::OpenclawBackend.capabilities().skills, "openclaw translates skills");

    let codex = crate::agents::codex::CodexBackend.capabilities();
    assert!(!codex.skills, "codex has no skills surface");
    assert!(!codex.plugins, "codex is not plugin-native");
}

/// The merged collapse is first-real-change-wins: a leading skip or NoOp never
/// masks a later change, and an all-NoOp fan-out stays NoOp.
#[test]
fn agent_report_merges_first_real_change() {
    let mut report = AgentReport::new();
    report.push("a", AgentStatus::Skipped(SkipReason::NotDetected));
    report.push("b", AgentStatus::Converged(Outcome::NoOp));
    report.push("c", AgentStatus::Converged(Outcome::Installed));
    report.push("d", AgentStatus::Converged(Outcome::Removed));
    assert_eq!(report.merged(), Outcome::Installed);

    let mut quiet = AgentReport::new();
    quiet.push("a", AgentStatus::Converged(Outcome::NoOp));
    assert_eq!(quiet.merged(), Outcome::NoOp);
}

/// The legacy single-`Outcome` methods collapse fail-at-end: the whole fan-out
/// ran, then the FIRST failed agent decides the `Err` (as `Error::Backend`), so
/// an exit-code consumer still learns about the failure.
#[test]
fn agent_report_into_merged_surfaces_the_first_failure() {
    let mut report = AgentReport::new();
    report.push("good", AgentStatus::Converged(Outcome::Installed));
    report.push("bad", AgentStatus::Failed("config unparseable".into()));
    report.push("worse", AgentStatus::Failed("second failure".into()));
    let err = match report.into_merged() {
        Err(e) => e,
        Ok(o) => panic!("a failed agent must surface as Err, got Ok({o:?})"),
    };
    assert_eq!(err.to_string(), "bad: config unparseable");

    let mut healthy = AgentReport::new();
    healthy.push("good", AgentStatus::Converged(Outcome::Installed));
    assert_eq!(healthy.into_merged().expect("no failure"), Outcome::Installed);
}

/// The report's Display is the `setup` summary a host prints verbatim: one line
/// per agent, end-user wording for outcomes, skips, and failures.
#[test]
fn agent_report_display_reads_as_a_setup_summary() {
    let mut report = AgentReport::new();
    report.push("claude", AgentStatus::Converged(Outcome::Updated { from: Some("0.1.0".into()), to: "0.2.0".into() }));
    report.push("codex", AgentStatus::Converged(Outcome::NoOp));
    report.push("zed", AgentStatus::Skipped(SkipReason::NotDetected));
    report.push("vscode-copilot", AgentStatus::Skipped(SkipReason::ScopeUnsupported));
    report.push("gemini", AgentStatus::Skipped(SkipReason::SourceUnsupported));
    report.push("goose", AgentStatus::Failed("boom".into()));
    assert_eq!(
        report.to_string(),
        "claude: updated (0.1.0 -> 0.2.0)\n\
         codex: no changes needed\n\
         zed: skipped (not installed on this machine)\n\
         vscode-copilot: skipped (no config surface at this scope)\n\
         gemini: skipped (cannot serve a github source; use an embedded or path source)\n\
         goose: failed: boom\n"
    );
    assert!(!report.is_healthy(), "a failed agent must flip is_healthy");
}

/// `Outcome`'s Display is end-user wording (`{outcome:?}` was the old example
/// output; `Updated { from: .. }` must never reach a user again).
#[test]
fn outcome_display_is_end_user_wording() {
    assert_eq!(Outcome::Installed.to_string(), "installed");
    assert_eq!(Outcome::Updated { from: None, to: "0.2.0".into() }.to_string(), "updated (to 0.2.0)");
    assert_eq!(Outcome::Adopted.to_string(), "adopted existing install");
    assert_eq!(Outcome::Cleared.to_string(), "cleared stale marker");
}
