//! Shared renderer/reconciler for the host-owned status-line slot, for every
//! harness whose slot lives in a JSON settings file. One module, N backends: a
//! caller supplies the settings file for a [`Scope`], the key path to its slot
//! (claude `["statusLine"]`, qwen-code `["ui","statusLine"]` — the last element is
//! the slot key, the leading ones are containers), its own client id, and the value
//! shape.
//!
//! This surface is the exception to every other one here. The rest merge our entries
//! beside the user's *by key*, so remove takes exactly ours back; a slot holds a
//! single value and is last-writer-wins, so writing ours necessarily displaces
//! theirs. Hence the whole lifecycle below: stash what we displaced into the stamp
//! marker BEFORE writing, restore it on remove, and decide ownership on the command
//! string alone. Full contract: `docs/design.md` § host-owned statusLine.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::BackendState;
use super::confedit::{json_edit, json_obj_at};
use crate::components::expand_client;
use crate::doctor::{CheckStatus, DoctorCheck};
use crate::error::{Error, Result};
use crate::host::{Plugin, Scope, Source};
use crate::stamp;
use crate::statusline::StatusLineDecl;

/// The body a harness's slot holds. Claude Code's shape is the only one implemented;
/// qwen-code copied it verbatim, and per `docs/research/statusline-survey.md` §3/§4 so
/// did droid and copilot-cli, both of which accept an optional `type: "command"` and
/// need no variant of their own. Only a genuinely different body earns one — droid's
/// `maxRows` is the single known candidate.
#[derive(Clone, Copy)]
pub(crate) enum ValueShape {
    /// `{type:"command", command, padding?}` — claude, qwen-code.
    ///
    /// `type` is a load-bearing discriminator, not decoration: qwen-code's own gate is
    /// `type === "command" && typeof command === "string" && command.trim().length > 0`,
    /// and anything failing it is re-read as a built-in PRESET, so the declared command
    /// never runs and nothing errors (`docs/research/statusline-survey.md` §1).
    TypedCommand,
}

#[derive(Clone, Copy)]
pub(crate) struct SlotShape {
    pub(crate) value: ValueShape,
}

impl SlotShape {
    /// CC's `{"type":"command","command":…}` object, plus `padding` when the host
    /// declared one.
    pub(crate) const fn typed_command() -> Self {
        Self { value: ValueShape::TypedCommand }
    }
}

/// Render one declaration per `shape`. A single object, never an array — the slot
/// holds exactly one value.
fn render(decl: &StatusLineDecl, shape: SlotShape) -> Value {
    match shape.value {
        ValueShape::TypedCommand => {
            let mut map = Map::new();
            map.insert("type".to_string(), Value::String("command".to_string()));
            map.insert("command".to_string(), Value::String(decl.command.clone()));
            if let Some(padding) = decl.padding {
                map.insert("padding".to_string(), Value::from(padding));
            }
            Value::Object(map)
        }
    }
}

/// The command string carried by whatever is in the slot now, whoever wrote it. The
/// one field ownership is decided on, so a shape whose body differs everywhere else
/// still answers "whose is this?" through the same test.
fn command_of(existing: &Value, shape: SlotShape) -> Option<&str> {
    match shape.value {
        ValueShape::TypedCommand => existing.get("command").and_then(Value::as_str),
    }
}

/// This host's status line rendered into `shape`, paired with the bare command
/// string it carries; both have `${AGENTGEAR_CLIENT}` expanded to `client`. `None`
/// when the host declares none, which makes every step below a no-op.
///
/// A blank command reads as "declares none" too. `StatusLineDecl::default()` carries
/// one, and rendering it would displace (and stash) the user's real status line in
/// exchange for a command that does nothing — a host bug that should cost them
/// nothing. Harnesses do not reject one either: qwen-code silently falls back to a
/// preset, so the write would look like it took and quietly show something else.
pub(crate) fn rendered(plugin: &Plugin, client: &str, shape: SlotShape) -> Option<(Value, String)> {
    let decl = plugin.statusline.as_ref()?;
    let command = expand_client(&decl.command, client);
    if command.trim().is_empty() {
        return None;
    }
    Some((render(&StatusLineDecl { command: command.clone(), ..decl.clone() }, shape), command))
}

/// The settings file a backend's slot lifecycle writes, or `None` when the host
/// declares no status line. `resolve` is the backend's own settings-path resolver and
/// runs only when there IS a declaration, so a declaration-free host never pays for —
/// or fails on — a config-dir lookup it has no use for.
pub(crate) fn target(
    plugin: &Plugin, client: &str, shape: SlotShape, resolve: impl FnOnce() -> Result<PathBuf>,
) -> Result<Option<PathBuf>> {
    if rendered(plugin, client, shape).is_none() {
        return Ok(None);
    }
    resolve().map(Some)
}

/// Whether the slot's live value is one of OUR renderings — matched on the command
/// string, NOT on the whole object. Deliberately a different test from the one
/// [`state`] and [`check`] use for convergence: any field drifting from what we
/// render is drift to repair, but only the command decides whose value it is.
///
/// Whole-value equality here would read our own earlier rendering as foreign the
/// moment a host release changes its padding. That misreading is not cosmetic:
/// reconcile would stash our command as "the user's original", destroying their real
/// value, and `compose` would then run the host binary from inside itself on every
/// turn.
///
/// Ceiling: a release that changes the COMMAND itself (renamed subcommand, new flag)
/// still reads as foreign, so it stashes its own old command and the user's value is
/// lost. The statusline module refuses to RUN a stash naming its own command, so the
/// worst case stays a missing row rather than re-entry. Upgrade path: write an
/// ownership key beside `command` — which needs each harness's settings schema proven
/// tolerant of an unknown key first.
pub(crate) fn is_ours(existing: &Value, our_command: &str, shape: SlotShape) -> bool {
    command_of(existing, shape) == Some(our_command)
}

/// Converge the harness's single slot to the host's declaration, returning whether
/// the settings file changed. A foreign value already in the slot is stashed verbatim
/// into the stamp marker so [`remove`] can put it back.
pub(crate) fn reconcile(
    path: &Path, key_path: &[&str], plugin: &Plugin, source: &Source, scope: &Scope, client: &str, shape: SlotShape,
) -> Result<bool> {
    let Some((ours, our_command)) = rendered(plugin, client, shape) else {
        return Ok(false);
    };
    let Some((containers, slot)) = slot_of(key_path) else {
        return Ok(false);
    };

    // Stash BEFORE writing: the write is irreversible, so recording what it displaced
    // only afterwards loses the user's value outright when the marker write fails
    // (ENOSPC, EPERM) or the process dies between the two. An empty slot writes no
    // stash at all, so "user deletes our line, self_heal re-adds it" cannot erase
    // what they had before we ever wrote.
    if let Some(existing) = read_settings(path)?.and_then(|root| value_at(&root, key_path).cloned())
        && !is_ours(&existing, &our_command, shape)
    {
        stamp::stash_statusline(plugin, scope, source, client, existing)?;
    }

    json_edit(path, |root| {
        let obj = json_obj_at(root, containers);
        if obj.get(slot) != Some(&ours) {
            obj.insert(slot.to_string(), ours.clone());
        }
        Ok(())
    })
}

/// Undo the slot write: restore the stashed original, or delete the slot key when
/// there was nothing to stash. Ownership is [`is_ours`] — the command string — so a
/// user who nudged only the padding on our line does not strand a command pointing at
/// the binary being uninstalled, while a genuinely foreign value is left exactly as
/// it is.
///
/// An unparseable settings file refuses the whole edit (`Error::Config`) rather than
/// clobbering it, so the stash is never consumed against a file that could not be read.
pub(crate) fn remove(path: &Path, key_path: &[&str], plugin: &Plugin, scope: &Scope, client: &str, shape: SlotShape) -> Result<bool> {
    let Some((_, our_command)) = rendered(plugin, client, shape) else {
        return Ok(false);
    };
    let Some((containers, slot)) = slot_of(key_path) else {
        return Ok(false);
    };
    let stashed = stamp::read(plugin, scope, client)?.and_then(|m| m.statusline_original);
    json_edit(path, |root| {
        // Navigate without creating: a settings file with no container object never
        // held a slot of ours, and creating one here would leave an empty `"ui": {}`
        // behind — a write, on a teardown that had nothing to undo.
        let Some(obj) = obj_at_mut(root, containers) else {
            return Ok(());
        };
        if !obj.get(slot).is_some_and(|existing| is_ours(existing, &our_command, shape)) {
            return Ok(());
        }
        match &stashed {
            Some(original) => obj.insert(slot.to_string(), original.clone()),
            None => obj.remove(slot),
        };
        Ok(())
    })
}

/// The slot surface's own state, or `None` when the host declares no status line
/// (contributing nothing to the backend's probe).
///
/// The two equality tests split the two questions, and the split is load-bearing:
///
/// - **presence** is [`is_ours`], the command string. A foreign value means OUR line
///   is not here, so it reports `Absent` — never `NeedsRepair`. A slot is the one
///   surface we do not own by key, so a status line someone else wrote is not
///   evidence our plugin is installed. Report it as present-but-drifted and
///   `report::compose` counts it as a present surface, which defeats the all-`Absent`
///   arm; self_heal's `(no marker, NeedsRepair)` adopt row then reinstalls the WHOLE
///   translation over a plugin the user deliberately removed.
/// - **convergence** is whole-value equality. Our own rendering with a drifted
///   `padding` is exactly the drift a repair exists to fix, so it is `NeedsRepair`
///   rather than `Healthy`.
///
/// The `Absent` arm still repairs where it should: a backend folding this in beside
/// its own owned surfaces reads present-siblings + `Absent` slot as a partial absence
/// (`NeedsRepair`), so a foreign line is retaken while the plugin is installed and
/// left alone once it is not. claude's fold is unaffected either way — it maps every
/// non-`Healthy` slot state onto `NeedsRepair` behind its registry's own presence gate.
///
/// An unparseable settings file reads as `Absent`; the reconcile that follows refuses
/// to clobber it and surfaces the parse error instead of silently overwriting the
/// user's file.
pub(crate) fn state(path: &Path, key_path: &[&str], plugin: &Plugin, client: &str, shape: SlotShape) -> Result<Option<BackendState>> {
    let Some((ours, our_command)) = rendered(plugin, client, shape) else {
        return Ok(None);
    };
    let root = read_settings(path)?;
    Ok(Some(match root.as_ref().and_then(|r| value_at(r, key_path)) {
        None => BackendState::Absent,
        Some(existing) if *existing == ours => BackendState::Healthy,
        Some(existing) if is_ours(existing, &our_command, shape) => BackendState::NeedsRepair,
        Some(_) => BackendState::Absent,
    }))
}

/// Parse a settings file for a read-only inspection: missing or unparseable both
/// read as "nothing to see", since neither is this function's to repair.
pub(crate) fn read_settings(path: &Path) -> Result<Option<Value>> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io { context: format!("reading {}", path.display()), source }),
    }
}

/// doctor's status-line slice, or `None` when the host declares no status line.
/// A foreign owner is a Warn, not a Fail: the slot holds one value, so losing it to
/// another tool is a real (and user-visible) state, not a broken install.
///
/// `settings` is the backend's already-resolved path as a `Result`, because doctor
/// reports rather than fails: a config-dir lookup that could not resolve becomes a
/// Warn here instead of taking the whole report down.
pub(crate) fn check(
    settings: Result<PathBuf>, key_path: &[&str], plugin: &Plugin, client: &str, shape: SlotShape, harness: &str,
) -> Option<DoctorCheck> {
    let name = "status line installed";
    let (ours, _) = rendered(plugin, client, shape)?;
    let slot = key_path.join(".");
    let path = match settings {
        Ok(path) => path,
        Err(e) => return Some(DoctorCheck { name, status: CheckStatus::Warn(format!("could not locate {harness}'s settings: {e}")) }),
    };
    let root = read_settings(&path).ok().flatten();
    Some(match root.as_ref().and_then(|r| value_at(r, key_path)) {
        Some(existing) if *existing == ours => {
            DoctorCheck { name, status: CheckStatus::Ok(format!("`{}` owns the {slot} slot", plugin.name)) }
        }
        Some(_) => DoctorCheck {
            name,
            status: CheckStatus::Warn(format!(
                "another status line owns `{slot}` in {}; the slot holds one value, so ours is not shown",
                path.display()
            )),
        },
        None => DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("no `{slot}` in {}", path.display()),
                fix: "run the host binary's `setup` (or `install`) subcommand".into(),
            },
        },
    })
}

/// Split a slot key path into the containers to walk and the slot key itself:
/// `["ui","statusLine"]` -> `(["ui"], "statusLine")`. Every caller passes a const, so
/// an empty path names no slot and is a caller bug rather than a runtime state.
fn slot_of<'a>(key_path: &'a [&'a str]) -> Option<(&'a [&'a str], &'a str)> {
    debug_assert!(!key_path.is_empty(), "a status-line key path must name its slot key");
    let (slot, containers) = key_path.split_last()?;
    Some((containers, slot))
}

/// Walk `key_path` into `root`, returning the value at the leaf. `[]` returns `root`.
fn value_at<'a>(root: &'a Value, key_path: &[&str]) -> Option<&'a Value> {
    let mut cur = root;
    for key in key_path {
        cur = cur.get(key)?;
    }
    Some(cur)
}

/// The mutable, NON-creating counterpart of [`value_at`] for the container path:
/// `None` when any level is missing or is not an object.
fn obj_at_mut<'a>(root: &'a mut Value, key_path: &[&str]) -> Option<&'a mut Map<String, Value>> {
    let mut cur = root;
    for key in key_path {
        cur = cur.get_mut(key)?;
    }
    cur.as_object_mut()
}

#[cfg(test)]
#[path = "../../tests/unit/statuslinejson.rs"]
mod statuslinejson_tests;
