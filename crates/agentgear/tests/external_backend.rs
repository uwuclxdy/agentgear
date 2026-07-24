//! Out-of-crate `AgentBackend` reachability: this file is its own crate, so it
//! compiles only if an external consumer can implement the trait end to end —
//! construct a real `DoctorReport` (via the public `from_checks`/`from_error`
//! constructors) and surface a real `Error` (the neutral `Error::Backend`
//! variant) without any `pub(crate)` escape hatch. The README/wiki advertise the
//! trait as unsealed; this test is what keeps that claim true.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agentgear::{
    AgentBackend, BackendState, Capabilities, CheckStatus, Desired, DoctorCheck, DoctorReport, Error, Outcome, Plugin, Result, Scope,
    Source,
};

/// A minimal external backend: every method constructible from public API only.
struct FakeAgent;

impl AgentBackend for FakeAgent {
    fn id(&self) -> &'static str {
        "fake"
    }

    fn detect(&self) -> bool {
        true
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: false,
            commands: false,
            agents: false,
            skills: false,
            instructions: false,
            statusline: false,
            scopes: &["user"],
        }
    }

    fn probe(&self, _plugin: &Plugin, _scope: &Scope, _source: &Source) -> Result<BackendState> {
        Ok(BackendState::Healthy)
    }

    fn reconcile(&self, _plugin: &Plugin, _desired: &Desired, _scope: &Scope) -> Result<Outcome> {
        // The neutral variant an external backend maps its own failures into.
        Err(Error::Backend { agent: "fake".into(), detail: "config write refused".into() })
    }

    fn remove(&self, _plugin: &Plugin, _scope: &Scope, _source: &Source) -> Result<Outcome> {
        Ok(Outcome::Removed)
    }

    fn report(&self, _plugin: &Plugin, _source: &Source) -> DoctorReport {
        DoctorReport::from_checks(vec![DoctorCheck {
            name: "fake backend reachable",
            status: CheckStatus::Ok("constructed out of crate".into()),
        }])
    }
}

#[test]
fn external_backend_constructs_report_and_error() {
    let fake = FakeAgent;
    assert_eq!(fake.id(), "fake");
    assert!(fake.capabilities().mcp);

    // `dyn` usability: the lifecycle stores backends as `Box<dyn AgentBackend>`.
    let boxed: Box<dyn AgentBackend> = Box::new(FakeAgent);
    assert!(boxed.detect());
}

#[test]
fn from_error_collapses_to_a_failed_check() {
    let report = DoctorReport::from_error(Error::Backend { agent: "fake".into(), detail: "boom".into() });
    assert!(!report.is_healthy());
    let rendered = report.to_string();
    assert!(rendered.contains("fake: boom"), "rendered: {rendered}");
}

#[test]
fn backend_error_renders_agent_and_detail() {
    let err = Error::Backend { agent: "fake".into(), detail: "config write refused".into() };
    assert_eq!(err.to_string(), "fake: config write refused");
}
