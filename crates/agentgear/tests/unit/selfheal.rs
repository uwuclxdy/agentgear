//! `skip_reason` unit tests: the two spawn-free skip gates the heal-needed
//! classifier mirrors from heal_status, exercised on their pure decision — a
//! github source cannot be reached through the embedded-default host fixture and
//! `detect()` is env-dependent, so the decision is the testable seam.

use crate::host::{Capabilities, Scope, SkipReason, Source};

use super::skip_reason;

fn capabilities(scopes: &'static [&'static str], plugins: bool) -> Capabilities {
    Capabilities { plugins, mcp: true, hooks: true, commands: true, agents: true, skills: true, instructions: false, scopes }
}

#[test]
fn project_only_backend_skips_at_user_scope() {
    let caps = capabilities(&["project"], false);
    assert_eq!(skip_reason(&caps, &Scope::User, &Source::Embedded), Some(SkipReason::ScopeUnsupported));
}

#[test]
fn github_source_on_a_config_backend_skips() {
    let caps = capabilities(&["user"], false);
    let github = Source::GitHub { repo: "owner/repo", ref_: "v0.1.0" };
    assert_eq!(skip_reason(&caps, &Scope::User, &github), Some(SkipReason::SourceUnsupported));
}

#[test]
fn github_source_on_a_plugin_native_backend_proceeds() {
    let caps = capabilities(&["user"], true);
    let github = Source::GitHub { repo: "owner/repo", ref_: "v0.1.0" };
    assert_eq!(skip_reason(&caps, &Scope::User, &github), None);
}

#[test]
fn embedded_source_on_a_config_backend_proceeds() {
    let caps = capabilities(&["user"], false);
    assert_eq!(skip_reason(&caps, &Scope::User, &Source::Embedded), None);
}
