//! `SessionSource` maps the `SessionStart` hook payload's `source` field, and the
//! retire decision it drives: only an event that began by loading plugin contents
//! from disk may retire the restart notice. `/compact` and `/clear` fire
//! `SessionStart` inside the session that already loaded the plugin — CC's hook
//! registration is memoized per session (probed 2.1.310) — so neither can prove
//! the running session holds the tree an update staged.

use super::SessionSource;

#[test]
fn every_documented_source_maps_and_an_unlisted_one_is_unknown() {
    let cases = [
        ("startup", SessionSource::Startup),
        ("resume", SessionSource::Resume),
        ("clear", SessionSource::Clear),
        ("compact", SessionSource::Compact),
        ("fork", SessionSource::Fork),
        ("", SessionSource::Unknown),
        ("relaunch", SessionSource::Unknown),
        ("Startup", SessionSource::Unknown),
    ];
    for (raw, expected) in cases {
        assert_eq!(SessionSource::from_hook_source(raw), expected, "source {raw:?}");
    }
}

#[test]
fn only_a_fresh_load_retires_the_notice() {
    let cases = [
        (SessionSource::Startup, true),
        (SessionSource::Resume, true),
        (SessionSource::Clear, false),
        (SessionSource::Compact, false),
        // A `--fork-session` process reloads the tree, but an in-process fork
        // would inherit the source session's registration and the payload does
        // not distinguish them; the notice outliving the event costs one session.
        (SessionSource::Fork, false),
        (SessionSource::Unknown, false),
    ];
    for (source, retires) in cases {
        assert_eq!(source.loaded_plugin_contents(), retires, "{source:?}");
    }
}

#[test]
fn a_hook_payload_maps_its_source_field() {
    let cases = [
        (r#"{"hook_event_name":"SessionStart","source":"compact"}"#, SessionSource::Compact),
        (r#"{"source":"startup"}"#, SessionSource::Startup),
        (r#"{"source":"resume","session_id":"abc"}"#, SessionSource::Resume),
        // Absent, malformed or non-CC payloads are "cannot tell" — never a
        // silent retire of a notice that may still be true.
        ("", SessionSource::Unknown),
        ("not json", SessionSource::Unknown),
        (r#"{"hook_event_name":"SessionStart"}"#, SessionSource::Unknown),
        (r#"{"source":42}"#, SessionSource::Unknown),
    ];
    for (payload, expected) in cases {
        assert_eq!(SessionSource::from_hook_payload(payload), expected, "payload {payload:?}");
    }
}
