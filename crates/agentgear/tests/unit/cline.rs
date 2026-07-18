//! cline unit tests: the hook-wrapper script embeds a CC hook `command` string
//! (arbitrary, user-authored shell source) into a generated bash script. Proven
//! by actually running the generated script through `bash`, not just inspecting
//! the source text, so the assertion is about real shell behavior, not this
//! module's own idea of what its escaping does.

use super::*;
use crate::components::HookBinding;

fn hook(command: &str) -> HookBinding {
    HookBinding { event: "UserPromptSubmit".into(), matcher: None, command: command.to_string() }
}

fn event_hook(event: &str) -> HookBinding {
    HookBinding { event: event.to_string(), matcher: None, command: "host_fixture check-restart".into() }
}

/// A command containing a space, single quotes, and a trailing `#` — the exact
/// class the fix guards: unquoted embedding lets the hook command's own bytes
/// splice into (or truncate) the wrapper's own trailing `2>/dev/null || true`,
/// since `#` opens a shell comment that swallows everything after it, including
/// the substitution's closing `)"`.
#[test]
fn hook_command_survives_the_wrapper_as_one_argument() {
    let h = hook("echo 'hi' # trailing comment breaks an unquoted embed");
    let script = render_hook_script("ez-plugin", &[&h]);

    let out = std::process::Command::new("bash").arg("-c").arg(&script).output().expect("spawn bash to run the generated hook script");
    assert!(out.status.success(), "generated hook script failed:\nstderr: {}\nscript:\n{script}", String::from_utf8_lossy(&out.stderr));

    let stdout = String::from_utf8_lossy(&out.stdout);
    let reply: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook script did not print JSON: {e}\nstdout: {stdout}\nscript:\n{script}"));
    assert_eq!(
        reply["contextModification"], "hi",
        "the hook command's own bytes corrupted the wrapper instead of running as one command:\nstdout: {stdout}\nscript:\n{script}"
    );
}

/// `PreCompact` is an exact name in cline's own file-hook enum (verify-cline.md #4),
/// so it maps 1:1 and the script lands as a bare-`PreCompact` file in the scanned
/// hooks dir. Guards the `map_event` arm: drop it and no `PreCompact` file is written.
#[test]
fn precompact_hook_maps_and_lands_in_the_scanned_dir() {
    assert_eq!(map_event("PreCompact"), Some("PreCompact"), "PreCompact must map to cline's identically-named event");

    let dir = std::env::temp_dir().join(format!("ez-cline-precompact-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let changed = reconcile_hooks(&dir, "ez-plugin", std::slice::from_ref(&event_hook("PreCompact"))).unwrap();
    assert!(changed, "writing the PreCompact hook must report a change");

    let script = dir.join("PreCompact");
    assert!(script.exists(), "PreCompact hook script did not land in the scanned dir: {}", script.display());
    let body = std::fs::read_to_string(&script).unwrap();
    assert!(body.contains("agentgear-managed:ez-plugin"), "PreCompact script missing ownership tag:\n{body}");
    assert!(body.contains("host_fixture check-restart"), "PreCompact script does not invoke the CC command:\n{body}");

    std::fs::remove_dir_all(&dir).ok();
}

/// CC's session-level `SessionEnd` maps onto cline's `SessionShutdown`: primary
/// source (`cline/cline` `sdk/.../hooks/subprocess.ts` `shutdown()`, called from the
/// memoized terminal cleanup in `run-agent.ts`/`session-runtime.ts`) fires
/// `session_shutdown` exactly once per session at teardown, carrying a `reason`, the
/// same shape as CC `SessionEnd`. The script lands under cline's OWN event name
/// (`SessionShutdown`), not the CC name. `SessionStart` stays skipped: cline has no
/// session-level start hook, only the per-task `TaskStart`.
#[test]
fn sessionend_maps_to_sessionshutdown_and_lands() {
    assert_eq!(map_event("SessionEnd"), Some("SessionShutdown"), "SessionEnd must map to cline's session-level SessionShutdown");
    assert_eq!(map_event("SessionStart"), None, "SessionStart has no cline session-level analog and must stay skipped");

    let dir = std::env::temp_dir().join(format!("ez-cline-sessionend-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let changed = reconcile_hooks(&dir, "ez-plugin", std::slice::from_ref(&event_hook("SessionEnd"))).unwrap();
    assert!(changed, "writing the SessionEnd hook must report a change");

    let script = dir.join("SessionShutdown");
    assert!(script.exists(), "SessionEnd hook must land under cline's SessionShutdown name: {}", script.display());
    assert!(!dir.join("SessionEnd").exists(), "the CC event name must not be used as the cline hook filename");
    let body = std::fs::read_to_string(&script).unwrap();
    assert!(body.contains("agentgear-managed:ez-plugin"), "SessionShutdown script missing ownership tag:\n{body}");
    assert!(body.contains("host_fixture check-restart"), "SessionShutdown script does not invoke the CC command:\n{body}");

    std::fs::remove_dir_all(&dir).ok();
}
