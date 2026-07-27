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
use super::confedit::{json_edit, json_obj_at, json_prune_obj, json_remove};
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
    /// A harness-local row cap written verbatim beside the command, when its slot has
    /// one. Not a host-declared knob: `StatusLineDecl` deliberately carries no
    /// `max_rows`, because exactly one harness has the field and its right value is a
    /// property of that harness's renderer, not of the host's declaration.
    pub(crate) max_rows: Option<u8>,
    /// Keys preserved VERBATIM off whatever is already in the slot, instead of being
    /// rendered by us. A deliberate per-field carry inside an otherwise whole-value
    /// write: the write replaces the object, so a field the harness owns and we do not
    /// model is destroyed unless it is named here.
    ///
    /// Preserving such a field is correct whether or not its render-time meaning is
    /// known: we do not interpret it, and we never synthesize one that was not there.
    pub(crate) carry: &'static [&'static str],
    /// What to tell a user whose carried switch is OFF. Only reachable through
    /// [`Self::carry`], so a non-carrying shape never renders it.
    pub(crate) disabled_hint: &'static str,
}

impl SlotShape {
    /// CC's `{"type":"command","command":…}` object, plus `padding` when the host
    /// declared one.
    pub(crate) const fn typed_command() -> Self {
        Self { value: ValueShape::TypedCommand, max_rows: None, carry: &[], disabled_hint: "" }
    }

    /// Preserve `keys` off the live value rather than rendering them
    /// (antigravity-cli's `enabled`). `hint` is the remedy doctor prints when one of
    /// them is the harness's own off-switch and it is off — the two travel together
    /// because a carried switch we refuse to override is only actionable if the user
    /// is told where it is.
    pub(crate) const fn carrying(mut self, keys: &'static [&'static str], hint: &'static str) -> Self {
        self.carry = keys;
        self.disabled_hint = hint;
        self
    }

    /// Emit `maxRows` beside the command (droid).
    pub(crate) const fn with_max_rows(mut self, rows: u8) -> Self {
        self.max_rows = Some(rows);
        self
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
            if let Some(rows) = shape.max_rows {
                map.insert("maxRows".to_string(), Value::from(rows));
            }
            Value::Object(map)
        }
    }
}

/// Our rendering with [`SlotShape::carry`] keys copied verbatim off `existing`. This
/// is the value BOTH the write and every convergence test use: render one and compare
/// the other and a carried field reads as permanent drift, so self_heal rewrites the
/// slot on every single pass.
fn with_carried(ours: &Value, existing: Option<&Value>, shape: SlotShape) -> Value {
    let mut out = ours.clone();
    if shape.carry.is_empty() {
        return out;
    }
    let Some(existing) = existing else {
        return out;
    };
    if let Some(obj) = out.as_object_mut() {
        for key in shape.carry {
            if let Some(value) = existing.get(*key) {
                obj.insert((*key).to_string(), value.clone());
            }
        }
    }
    out
}

/// A carried key sitting at `false` in the live value: the harness's own on/off switch
/// for this slot, switched off. Ours is installed and converged and the harness will
/// render none of it — a state that is otherwise completely silent, since the write
/// succeeded, `state` reads `Healthy`, and every lifecycle op is a clean `NoOp`.
///
/// Keyed on the carry rather than on any one harness's field name, so a later carrying
/// backend inherits the check. A carried key holding anything but `false` (a non-bool,
/// or `true`) is not a switch we can read as off.
fn disabled_carry(existing: &Value, shape: SlotShape) -> Option<&'static str> {
    shape.carry.iter().copied().find(|key| existing.get(*key) == Some(&Value::Bool(false)))
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
/// `last_written` is the command the stamp marker says this backend last put in the
/// slot (`Marker::statusline_command`), and it is what makes ownership survive a host
/// renaming its own status-line subcommand: the value we wrote at the old name matches
/// neither the new declaration nor anything derivable from it, so without the record it
/// reads as foreign and gets stashed over the user's real original. `None` — every
/// install predating the record — falls back to the current-command compare alone. A
/// rename over one of those still stashes our own previous command, and the guard that
/// is supposed to refuse to RUN such a stash has a ceiling of its own; it is written
/// out at `statusline::is_own_command`.
///
/// A recorded command only ever widens ownership onto a value whose command string is
/// literally that record, and the record is written only after the settings write it
/// names succeeded, so it can never claim a command that did not reach the slot.
pub(crate) fn is_ours(existing: &Value, our_command: &str, last_written: Option<&str>, shape: SlotShape) -> bool {
    let Some(command) = command_of(existing, shape) else {
        return false;
    };
    command == our_command || last_written == Some(command)
}

/// The command this backend last wrote into `scope`'s slot, per the stamp marker.
fn last_written(marker: Option<&stamp::Marker>) -> Option<&str> {
    marker.and_then(|m| m.statusline_command.as_deref())
}

/// Converge the harness's single slot to the host's declaration, returning whether
/// the settings file changed. A foreign value already in the slot is stashed verbatim
/// into the stamp marker so [`remove`] can put it back, and the command we wrote is
/// recorded there too so a later release that renames it still reads the value as ours.
pub(crate) fn reconcile(
    path: &Path, key_path: &[&str], plugin: &Plugin, source: &Source, scope: &Scope, client: &str, shape: SlotShape,
) -> Result<bool> {
    let Some((ours, our_command)) = rendered(plugin, client, shape) else {
        return Ok(false);
    };
    let Some((containers, slot)) = slot_of(key_path) else {
        return Ok(false);
    };

    // Three orderings around the settings write, each load-bearing for its own reason
    // and only the first defended by a test, so keep them apart when editing this:
    //
    // - the READ must precede the settings write. Afterwards the slot reads as ours,
    //   `is_ours` is true, and nothing is ever stashed, so the restore has nothing to
    //   put back. Moving this block below `json_edit` reds most of the claude suite.
    // - the STASH must precede the settings write. That leaves a crash window — an
    //   ENOSPC/EPERM/kill between the two loses the user's value outright — but the
    //   other order loses it on every run, not just a crashing one. Hoisting only the
    //   read and stashing afterwards is invisible to every test, so nothing but this
    //   comment holds it.
    // - the COMMAND RECORD must FOLLOW a settings write that succeeded, which is the
    //   opposite direction because its failure mode is the opposite one. `json_edit`
    //   refuses an unparseable settings file (`Error::Config`) while `read_settings`
    //   above reads that same file as an empty slot, so recording first overwrites a
    //   TRUE record of what is still sitting in the slot with a command that never got
    //   written — and the next rename then reads the real value as foreign and stashes
    //   it over the user's original, which is exactly the loss this record exists to
    //   stop. Recording after needs a crash between the two writes AND a further rename
    //   before any successful reconcile; recording before needs one parse error.
    //
    // An empty slot writes no stash at all, so "user deletes our line, self_heal
    // re-adds it" cannot erase what they had before we ever wrote. The command record
    // is unconditional: tying it to the stash would leave every install that took an
    // EMPTY slot with no record, and those renames lose nothing but still misread.
    let marker = stamp::read(plugin, scope, client)?;
    let existing = read_settings(path)?.and_then(|root| value_at(&root, key_path).cloned());
    if let Some(displaced) = existing.clone()
        && !is_ours(&displaced, &our_command, last_written(marker.as_ref()), shape)
    {
        stamp::stash_statusline(plugin, scope, source, client, displaced)?;
    }

    // Carry the harness-owned keys off the value we are replacing, so taking the slot
    // over does not reset a preference of theirs we do not model. With the slot GONE
    // (the user deleted our line and self_heal is re-adding it) the stash is the last
    // record of that preference, so it is the fallback — re-adding without it resets
    // exactly what the carry exists to protect. Never worse than carrying nothing: the
    // stash either holds the key, or it does not and we are back to writing none. Read
    // off the marker as it was BEFORE the stash above, which is the same value here: the
    // two are mutually exclusive, since the stash fires only on a live slot and this
    // fallback arm only on an absent one.
    let carry_source = match &existing {
        Some(_) => existing.clone(),
        None if !shape.carry.is_empty() => marker.and_then(|m| m.statusline_original),
        None => None,
    };
    let ours = with_carried(&ours, carry_source.as_ref(), shape);
    let changed = json_edit(path, |root| {
        let obj = json_obj_at(root, containers);
        if obj.get(slot) != Some(&ours) {
            obj.insert(slot.to_string(), ours.clone());
        }
        Ok(())
    })?;
    // Unconditional on `changed`: a no-op write still proves the slot holds this
    // command, and an install predating the record reaches its very first pass with
    // that command ALREADY in the slot — so gating this on `changed` would leave
    // exactly the population this record exists for without one, forever. Pinned by
    // `claude_statusline_a_converged_slot_still_records_its_command`.
    stamp::record_statusline_command(plugin, scope, source, client, &our_command)?;
    Ok(changed)
}

/// Undo the slot write: restore the stashed original, or delete the slot key when
/// there was nothing to stash. Ownership is [`is_ours`] — the declared command string,
/// or the one the marker records we last wrote — so a user who nudged only the padding
/// on our line does not strand a command pointing at the binary being uninstalled, and
/// neither does an uninstall run by a release that renamed its own subcommand after the
/// slot was last written. A genuinely foreign value is left exactly as it is.
///
/// An unparseable settings file refuses the whole edit (`Error::Config`) rather than
/// clobbering it, so the stash is never consumed against a file that could not be read.
///
/// A settings file left holding nothing at all is dropped: it was ours to begin with.
pub(crate) fn remove(path: &Path, key_path: &[&str], plugin: &Plugin, scope: &Scope, client: &str, shape: SlotShape) -> Result<bool> {
    let Some((_, our_command)) = rendered(plugin, client, shape) else {
        return Ok(false);
    };
    let Some((containers, slot)) = slot_of(key_path) else {
        return Ok(false);
    };
    let marker = stamp::read(plugin, scope, client)?;
    let last = last_written(marker.as_ref()).map(str::to_string);
    let stashed = marker.and_then(|m| m.statusline_original);
    json_remove(path, |root| {
        // Navigates without creating, and takes the container back out when our slot
        // key is what emptied it: reconcile created that container, so leaving an
        // empty `"ui": {}` behind would be a write on a teardown that had nothing to
        // undo. A restore refills the slot, so the prune only fires on the drop arm.
        json_prune_obj(root, containers, |obj| {
            if !obj.get(slot).is_some_and(|existing| is_ours(existing, &our_command, last.as_deref(), shape)) {
                return Ok(());
            }
            match &stashed {
                Some(original) => obj.insert(slot.to_string(), original.clone()),
                None => obj.remove(slot),
            };
            Ok(())
        })
        .map(|_| ())
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
///   translation over a plugin the user deliberately removed. The marker's recorded
///   command widens that presence test without widening this hole: the record and the
///   marker self_heal reads `has_marker` off are the same file, so the no-marker half
///   of that adopt row can only ever see the current-command compare.
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
pub(crate) fn state(
    path: &Path, key_path: &[&str], plugin: &Plugin, scope: &Scope, client: &str, shape: SlotShape,
) -> Result<Option<BackendState>> {
    let Some((ours, our_command)) = rendered(plugin, client, shape) else {
        return Ok(None);
    };
    let marker = stamp::read(plugin, scope, client)?;
    let root = read_settings(path)?;
    Ok(Some(match root.as_ref().and_then(|r| value_at(r, key_path)) {
        None => BackendState::Absent,
        // Compared against the value reconcile WOULD write for this live slot, carry
        // included — otherwise a carried field is drift that never converges.
        Some(existing) if *existing == with_carried(&ours, Some(existing), shape) => BackendState::Healthy,
        Some(existing) if is_ours(existing, &our_command, last_written(marker.as_ref()), shape) => BackendState::NeedsRepair,
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
/// `resolve` is the backend's own settings-path resolver and is run here, against the
/// `scope` the marker is also read at, so the two cannot disagree — the same shape
/// [`target`] uses, and the reason neither is passed as a bare path. It runs only when
/// there IS a declaration, so a declaration-free host never pays for a config-dir
/// lookup it has no use for.
///
/// Its failure is reported rather than propagated, because doctor reports rather than
/// fails: a config-dir lookup that could not resolve GENUINELY (no HOME) becomes a Warn
/// here instead of taking the whole report down. An empty config-dir override is
/// different: `reconcile`/`remove` hard-reject it (see each backend's
/// `ensure_statusline_resolves`), so a Warn here would contradict an install that just
/// failed on the exact same condition — this one variant is a Fail instead, matched on
/// the type rather than its rendered message.
///
/// The scope buys the same widened ownership [`state`] uses, so that the first doctor
/// run after a release renames its own status-line subcommand does not report the
/// user's own binary's previous command as another tool having taken the slot. A marker
/// read that fails degrades to the current-command compare rather than taking the report
/// down, matching this function's reports-rather-than-fails posture.
pub(crate) fn check(
    key_path: &[&str], plugin: &Plugin, scope: &Scope, client: &str, shape: SlotShape, harness: &str,
    resolve: impl FnOnce(&Scope) -> Result<PathBuf>,
) -> Option<DoctorCheck> {
    let name = "status line installed";
    let (ours, our_command) = rendered(plugin, client, shape)?;
    let slot = key_path.join(".");
    let path = match resolve(scope) {
        Ok(path) => path,
        Err(Error::EmptyConfigDirOverride { var }) => {
            return Some(DoctorCheck {
                name,
                status: CheckStatus::Fail {
                    problem: format!("`{var}` is set to an empty string, so {harness}'s config dir cannot be resolved"),
                    fix: format!("unset `{var}` or point it at a real directory"),
                },
            });
        }
        Err(e) => return Some(DoctorCheck { name, status: CheckStatus::Warn(format!("could not locate {harness}'s settings: {e}")) }),
    };
    let marker = stamp::read(plugin, scope, client).ok().flatten();
    let last = last_written(marker.as_ref());
    let root = read_settings(&path).ok().flatten();
    Some(match root.as_ref().and_then(|r| value_at(r, key_path)) {
        // Same carried comparison `state` uses, or doctor reports a converged slot as
        // someone else's.
        Some(existing) if *existing == with_carried(&ours, Some(existing), shape) => match disabled_carry(existing, shape) {
            // Installed and converged, but the harness's own switch is off, so the user
            // sees an empty bar with nothing anywhere explaining why. We deliberately do
            // not flip it back (it is their preference); saying so is the whole remedy.
            Some(key) => DoctorCheck {
                name,
                status: CheckStatus::Warn(format!(
                    "`{}` owns the {slot} slot, but `{key}` is false there: {harness}'s own status-line switch is off, so none of it renders. {}",
                    plugin.name, shape.disabled_hint
                )),
            },
            None => DoctorCheck { name, status: CheckStatus::Ok(format!("`{}` owns the {slot} slot", plugin.name)) },
        },
        // Ours by command but not converged whole-value: the value a previous release
        // (or a previous subcommand name) left there, plus anything the user nudged on
        // our line. It reads as drift for `state` and gets repaired by the next
        // reconcile, so calling it another tool's line here is simply wrong.
        Some(existing) if is_ours(existing, &our_command, last, shape) => DoctorCheck {
            name,
            status: CheckStatus::Warn(format!(
                "`{}` owns the {slot} slot in {}, but its value has drifted from what this version writes",
                plugin.name,
                path.display()
            )),
        },
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

#[cfg(test)]
#[path = "../../tests/unit/statuslinejson.rs"]
mod statuslinejson_tests;
