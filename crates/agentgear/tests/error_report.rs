//! `Error::report` renders the one line a host prints: the context and every
//! cause, joined with `: `.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agentgear::Error;
use std::io;

#[test]
fn report_joins_the_context_and_its_cause() {
    let err = Error::Io { context: "reading /x".to_string(), source: io::Error::new(io::ErrorKind::NotFound, "boom") };
    assert_eq!(err.report(), "reading /x: boom");
}

#[test]
fn report_renders_a_causeless_variant_as_its_own_display() {
    assert_eq!(Error::Tree("no plugin.json".to_string()).report(), "invalid plugin tree: no plugin.json");
}
