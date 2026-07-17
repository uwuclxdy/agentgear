//! Shared Claude-Code-shaped hook helpers for the non-CC backends. Several
//! harnesses (augment, gemini, codex, devin, droid, goose, qwen-code) reuse CC's
//! own nested hook object verbatim — `{matcher?, hooks:[{type:"command", command}]}`
//! — so the render lives here once instead of being copied per backend.

use serde_json::{Map, Value};

use crate::components::HookBinding;

/// Render one CC-shaped hook group: an optional `matcher`, then a single
/// `command`-type handler carrying the hook's command. This is CC's own on-disk
/// shape, which the harnesses above accept unchanged.
pub(crate) fn render_hook_group(hook: &HookBinding) -> Value {
    let mut group = Map::new();
    if let Some(matcher) = &hook.matcher {
        group.insert("matcher".into(), Value::from(matcher.clone()));
    }
    let mut handler = Map::new();
    handler.insert("type".into(), Value::from("command"));
    handler.insert("command".into(), Value::from(hook.command.clone()));
    group.insert("hooks".into(), Value::Array(vec![Value::Object(handler)]));
    Value::Object(group)
}

/// Free-fn form of [`HookBinding::is_portable`], the single portability predicate a
/// backend applies before writing a hook. Kept as a thin forwarder so each backend
/// references one shared name instead of a per-file copy.
pub(crate) fn hook_is_portable(hook: &HookBinding) -> bool {
    hook.is_portable()
}
