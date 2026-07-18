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
