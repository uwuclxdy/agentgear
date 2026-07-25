//! Shared Claude-Code-shaped hook helpers for the non-CC backends. Several
//! harnesses (augment, gemini, codex, devin, droid, goose, qwen-code) reuse CC's
//! own nested hook object verbatim — `{matcher?, hooks:[{type:"command", command}]}`
//! — so the render lives here once instead of being copied per backend.

use std::collections::BTreeSet;

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

/// Strip the handlers whose command is in `ours` out of every CC-shaped group under
/// `events`, dropping a group and an event array only when OUR removal is what emptied
/// it. Shared by every backend rendering CC's nested `hooks.<event>[].hooks[]` shape,
/// so the ownership rule cannot drift between them.
///
/// The same rule [`super::confedit::json_prune_at`] runs on containers, two levels
/// down: emptiness is measured ACROSS our edit, never after it. A group or event array
/// the user already had empty is theirs and survives — sweeping every empty one
/// instead deletes their entry, and (since the emptied `hooks` container then prunes,
/// and the emptied root then takes the file) their whole settings file with it.
pub(crate) fn remove_hook_groups(events: &mut Map<String, Value>, ours: &BTreeSet<&str>) {
    events.retain(|_, groups| {
        let Some(list) = groups.as_array_mut() else { return true };
        if list.is_empty() {
            return true;
        }
        list.retain_mut(|group| {
            let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else { return true };
            if handlers.is_empty() {
                return true;
            }
            handlers.retain(|h| h.get("command").and_then(Value::as_str).is_none_or(|c| !ours.contains(c)));
            !handlers.is_empty()
        });
        !list.is_empty()
    });
}

/// Free-fn form of [`HookBinding::is_portable`], the single portability predicate a
/// backend applies before writing a hook. Kept as a thin forwarder so each backend
/// references one shared name instead of a per-file copy.
pub(crate) fn hook_is_portable(hook: &HookBinding) -> bool {
    hook.is_portable()
}
