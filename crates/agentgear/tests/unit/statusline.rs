//! statusline unit tests: reading a stashed value back, session-cwd extraction, and
//! the user-command runner's stdin/trim/empty contract. What a declaration RENDERS
//! into a harness slot belongs to the shared slot renderer now
//! (`tests/unit/statuslinejson.rs`). The disk-touching half (marker stash, settings
//! write) routes through `data_root`/`dirs::data_dir`, so it is covered by the
//! host-fixture hermetic tests instead.

use serde_json::{Value, json};

use super::{StatusLineDecl, is_own_command, lookup_scopes, session_cwd};
use crate::host::Scope;

#[test]
fn from_value_reads_the_slot_object_shape() {
    // A literal pin rather than a round-trip through our own renderer: this reads
    // what a HARNESS has on disk, so it must not move when our rendering does.
    let back = StatusLineDecl::from_value(&json!({"type": "command", "command": "their-bar --fancy", "padding": 1}))
        .expect("a slot object must read back");
    assert_eq!(back, StatusLineDecl::new("their-bar --fancy").with_padding(1));
}

#[test]
fn from_value_reads_a_foreign_shape_with_extra_keys() {
    // The stash is kept verbatim in whatever shape the harness used, so an unknown
    // sibling key must not stop us running the command on restore.
    let raw: Value = json!({"type": "command", "command": "their-bar", "somethingElse": true});
    let decl = StatusLineDecl::from_value(&raw).expect("an extra key must not block the read");
    assert_eq!(decl.command, "their-bar");
    assert_eq!(decl.padding, None);
}

#[test]
fn from_value_rejects_a_shape_without_a_command() {
    assert_eq!(StatusLineDecl::from_value(&json!({"type": "command"})), None);
    assert_eq!(StatusLineDecl::from_value(&json!({"command": 7})), None);
    assert_eq!(StatusLineDecl::from_value(&json!("their-bar")), None);
}

#[test]
fn session_cwd_prefers_the_top_level_key() {
    let json = r#"{"cwd": "/work/proj", "workspace": {"current_dir": "/other"}}"#;
    assert_eq!(session_cwd(json).as_deref(), Some(std::path::Path::new("/work/proj")));
}

#[test]
fn session_cwd_falls_back_to_the_workspace_block() {
    let json = r#"{"workspace": {"current_dir": "/work/proj"}}"#;
    assert_eq!(session_cwd(json).as_deref(), Some(std::path::Path::new("/work/proj")));
}

#[test]
fn session_cwd_is_none_without_a_directory() {
    assert_eq!(session_cwd("{}"), None);
    assert_eq!(session_cwd("not json at all"), None);
    assert_eq!(session_cwd(r#"{"cwd": 7}"#), None);
}

#[test]
fn lookup_scopes_tries_project_before_user() {
    // The harness's own precedence is whole-value project-over-user, so a project
    // install's stash must shadow the user one, never the other way round.
    let scopes = lookup_scopes(Some(std::path::Path::new("/work/proj")));
    assert_eq!(scopes.len(), 2, "a cwd must yield project then user: {scopes:?}");
    match &scopes[0] {
        Scope::Project { path } => assert_eq!(path, std::path::Path::new("/work/proj")),
        other => panic!("project scope must come first, got {other:?}"),
    }
    assert!(matches!(scopes[1], Scope::User), "user scope must be the fallback, got {:?}", scopes[1]);
}

#[test]
fn lookup_scopes_without_a_cwd_is_user_only() {
    let scopes = lookup_scopes(None);
    assert_eq!(scopes.len(), 1, "no cwd means no project scope to consult: {scopes:?}");
    assert!(matches!(scopes[0], Scope::User));
}

#[test]
fn a_stash_naming_our_own_command_is_refused() {
    // The anti-recursion guard. A marker that stashed our own command (written by a
    // binary whose ownership test was wrong, or by another process) would otherwise
    // make compose spawn the very binary it runs inside, on every rendered turn.
    let ours = "mytool statusline --client claude";
    assert!(is_own_command(&StatusLineDecl::new(ours), Some(ours)));
    // Padding is irrelevant: the command is what gets spawned.
    assert!(is_own_command(&StatusLineDecl::new(ours).with_padding(4), Some(ours)));
}

#[test]
fn a_stash_naming_a_different_command_is_kept() {
    let ours = "mytool statusline --client claude";
    assert!(!is_own_command(&StatusLineDecl::new("their-bar"), Some(ours)));
    // A host that declares no status line has nothing to recurse into.
    assert!(!is_own_command(&StatusLineDecl::new("their-bar"), None));
    assert!(!is_own_command(&StatusLineDecl::new(ours), None));
}

// The runner spawns the platform shell; the assertions below use POSIX commands.
#[cfg(not(windows))]
mod runner {
    use std::time::{Duration, Instant};

    use super::super::{run_status_command, run_with_timeout};

    #[test]
    fn user_command_receives_the_session_json_on_stdin() {
        let out = run_status_command("cat", r#"{"cwd":"/work"}"#);
        assert_eq!(out.as_deref(), Some(r#"{"cwd":"/work"}"#), "the session json must reach the command's stdin");
    }

    #[test]
    fn user_command_output_keeps_interior_lines_and_drops_the_trailing_newline() {
        let out = run_status_command("printf 'one\\ntwo\\n'", "{}");
        assert_eq!(out.as_deref(), Some("one\ntwo"), "multi-line output is multi-row; only the trailing newline goes");
    }

    #[test]
    fn a_command_that_cannot_run_contributes_nothing() {
        // Exit 127 with empty stdout: a broken user command must not blank our bar.
        assert_eq!(run_status_command("agentgear-no-such-command-exists", "{}"), None);
    }

    #[test]
    fn empty_output_contributes_nothing() {
        assert_eq!(run_status_command("true", "{}"), None);
    }

    #[test]
    fn a_nonzero_exit_that_still_printed_is_kept() {
        // The harness renders whatever a status command prints, exit code ignored.
        assert_eq!(run_status_command("printf 'partial'; exit 3", "{}").as_deref(), Some("partial"));
    }

    #[test]
    fn a_hung_command_is_killed_and_contributes_nothing() {
        // Our rows are nested inside the harness's own status-line budget, so a user
        // command that never exits must cost a bounded wait, not the whole bar.
        let started = Instant::now();
        let out = run_with_timeout("sleep 60", "{}", Duration::from_millis(300));
        let elapsed = started.elapsed();
        assert_eq!(out, None, "a hung command must contribute nothing");
        assert!(elapsed < Duration::from_secs(5), "the timeout did not bound the wait: {elapsed:?}");
    }

    #[test]
    fn a_session_payload_larger_than_the_pipe_buffer_does_not_deadlock() {
        // 200 KB against a 64 KB pipe: writing the whole payload before reading
        // stdout blocks both sides forever when the child never drains stdin.
        let payload = "x".repeat(200_000);
        let out = run_with_timeout("echo ok", &payload, Duration::from_secs(10));
        assert_eq!(out.as_deref(), Some("ok"), "a large session payload must not stall the read");
    }

    #[test]
    fn runaway_output_is_cut_at_the_ceiling_instead_of_running_to_the_timeout() {
        // `yes` never stops. Unbounded, the reader allocates until the timeout fires
        // and the row is lost entirely; bounded, it returns immediately with a cut
        // row. The short timeout is what makes the difference observable.
        let started = Instant::now();
        let out = run_with_timeout("yes agentgear", "{}", Duration::from_secs(2));
        let text = out.expect("a runaway command must still yield its first rows, not time out");
        assert!(text.len() as u64 <= super::super::MAX_OUTPUT_BYTES, "read past the ceiling: {} bytes", text.len());
        assert!(started.elapsed() < Duration::from_secs(2), "the ceiling did not short-circuit the wait");
    }

    #[test]
    fn output_larger_than_the_pipe_buffer_is_read_whole() {
        // The mirror case: the child outruns the pipe buffer while we are still
        // writing stdin.
        let out = run_with_timeout("yes agentgear | head -n 20000", "{}", Duration::from_secs(10));
        let text = out.expect("a large output must be collected, not truncated to nothing");
        assert_eq!(text.lines().count(), 20_000, "output was truncated: {} bytes", text.len());
    }
}
