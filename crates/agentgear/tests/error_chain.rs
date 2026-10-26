//! A chain-aware renderer must not print a variant's cause twice: `Display` names
//! the operation, `source()` carries the cause, and anyhow's `{:#}` joins the two
//! once.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agentgear::Error;
use std::io;

#[test]
fn io_variant_names_its_cause_once_in_an_anyhow_chain() {
    let err = Error::Io { context: "reading /x".to_string(), source: io::Error::new(io::ErrorKind::NotFound, "boom") };
    assert_eq!(format!("{:#}", anyhow::Error::from(err)), "reading /x: boom");
}

#[test]
fn json_variant_names_its_cause_once_in_an_anyhow_chain() {
    let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    let rendered = source.to_string();
    let err = Error::Json { what: "plugin.json".to_string(), source };
    assert_eq!(format!("{:#}", anyhow::Error::from(err)), format!("could not parse plugin.json as JSON: {rendered}"));
}

#[test]
fn lock_variant_names_its_cause_once_in_an_anyhow_chain() {
    let err = Error::Lock { path: "/l".into(), source: io::Error::new(io::ErrorKind::WouldBlock, "held") };
    assert_eq!(format!("{:#}", anyhow::Error::from(err)), "failed to acquire the shared lock at /l: held");
}
