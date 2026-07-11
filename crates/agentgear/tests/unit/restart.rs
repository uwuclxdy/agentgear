//! restart unit tests: only the pure `message` builder is unit-testable here.
//! The `set`/`clear`/`pending` flag file helpers route through `data_root`
//! (`dirs::data_dir`), which can't be redirected without mutating process env —
//! forbidden in edition 2024 — so their lifecycle is covered by the host-fixture
//! e2e (`restart_flag_lifecycle`), matching how `stamp.rs` is tested.

use crate::restart::message;

#[test]
fn message_names_plugin_version_and_remedy() {
    let m = message("claudix", "1.2.3");
    assert!(m.contains("claudix"), "message must name the plugin: {m}");
    assert!(m.contains("1.2.3"), "message must name the version: {m}");
    assert!(m.contains("/reload-plugins"), "message must name the preferred remedy: {m}");
}

#[test]
fn message_is_factual_not_imperative() {
    // CC's prompt-injection defenses flag imperative framing ("restart now",
    // "ask the user to…"); the message must read as environment state instead.
    let m = message("p", "0");
    assert!(!m.contains("Ask the user"), "imperative phrasing trips injection defenses: {m}");
    assert!(!m.contains("Do not"), "imperative phrasing trips injection defenses: {m}");
}
