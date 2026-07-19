//! Shared `doctor` report helpers for the non-CC backends. A backend's `report`
//! assembles a `Vec<DoctorCheck>`; the read-parse boilerplate (open a JSON config,
//! parse it, read the plugin components) and the two mcp checks (server registered,
//! command on PATH) are identical across the config-family backends, so they live
//! here once. A harness with special wording (amp's `settings.jsonc`, openclaw's
//! JSON5, codex/goose's non-JSON configs) keeps its own inline read.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::BackendState;
use crate::components::{HookBinding, McpKind, McpServer, PluginComponents};
use crate::doctor::{CheckStatus, DoctorCheck};
use crate::error::{Error, Result};
use crate::host::{Plugin, Source};

/// Read + parse a JSON config file for a doctor report: push exactly one status
/// check under `name` (parses / does-not-parse / missing / unreadable) and return
/// the parsed root, `None` on any non-Ok outcome.
pub(crate) fn read_json_config(checks: &mut Vec<DoctorCheck>, name: &'static str, path: &Path) -> Option<Value> {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name, status: CheckStatus::Ok(format!("{} parses", path.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name,
                    status: CheckStatus::Fail {
                        problem: format!("{} does not parse: {e}", path.display()),
                        fix: "fix the JSON syntax or remove the file".into(),
                    },
                });
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            checks.push(DoctorCheck { name, status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", path.display())) });
            None
        }
        Err(e) => {
            checks.push(DoctorCheck { name, status: CheckStatus::Warn(format!("could not read {}: {e}", path.display())) });
            None
        }
    }
}

/// Read the plugin's components IR for a doctor report. On error push a Fail check
/// and return `None` so the caller can early-return the report it has so far.
pub(crate) fn components(checks: &mut Vec<DoctorCheck>, plugin: &Plugin, source: &Source) -> Option<PluginComponents> {
    match plugin.components(source) {
        Ok(comp) => Some(comp),
        Err(e) => {
            checks.push(DoctorCheck {
                name: "plugin components",
                status: CheckStatus::Fail {
                    problem: format!("could not read the plugin tree: {e}"),
                    fix: "rebuild the host binary".into(),
                },
            });
            None
        }
    }
}

/// Why a declared entry will never be written. A `${CLAUDE_PLUGIN_ROOT}` command
/// reaches the harness verbatim, so the filter drops it; a transport the harness
/// has no shape for is dropped rather than written wrong.
pub(crate) const NON_PORTABLE: &str = "${CLAUDE_PLUGIN_ROOT} expands only inside Claude Code (use a bare command name)";
pub(crate) const UNRENDERABLE: &str = "this harness cannot host that transport";

/// One entry the plugin declared that this backend will never write.
pub(crate) struct Skipped<'a> {
    pub(crate) name: &'a str,
    pub(crate) why: &'static str,
}

/// The declared servers missing from `writable`, each tagged with why it was
/// dropped. A backend passes the names its own filters left it with.
pub(crate) fn skipped_mcp<'a>(servers: &'a [McpServer], writable: &[&str]) -> Vec<Skipped<'a>> {
    servers
        .iter()
        .filter(|s| !writable.contains(&s.name.as_str()))
        .map(|s| Skipped { name: &s.name, why: if s.is_portable() { UNRENDERABLE } else { NON_PORTABLE } })
        .collect()
}

/// Fold the entries this backend will never write into `check`, downgrading a
/// passing check to `Warn` and naming them grouped by reason. Without it a plugin
/// whose entries were all dropped installs clean, does nothing, and still reads
/// `Ok`. A `Fail` passes through: a real breakage outranks the note, and each
/// harness keeps its own success wording.
pub(crate) fn note_skipped(check: DoctorCheck, skipped: &[Skipped<'_>]) -> DoctorCheck {
    let detail = match &check.status {
        _ if skipped.is_empty() => return check,
        CheckStatus::Fail { .. } => return check,
        CheckStatus::Ok(detail) | CheckStatus::Warn(detail) => detail.clone(),
    };
    let mut groups: Vec<(&'static str, Vec<&str>)> = Vec::new();
    for entry in skipped {
        match groups.iter_mut().find(|(why, _)| *why == entry.why) {
            Some((_, names)) => names.push(entry.name),
            None => groups.push((entry.why, vec![entry.name])),
        }
    }
    let dropped = groups.iter().map(|(why, names)| format!("skipped {}: {why}", names.join(", "))).collect::<Vec<_>>().join("; ");
    DoctorCheck { name: check.name, status: CheckStatus::Warn(format!("{detail}; {dropped}")) }
}

/// The hooks whose command carries `${CLAUDE_PLUGIN_ROOT}`, keyed by event. An
/// event with no analog on this harness is a documented per-backend skip, so it
/// is deliberately not in here.
pub(crate) fn skipped_hooks(hooks: &[HookBinding]) -> Vec<Skipped<'_>> {
    hooks
        .iter()
        .filter(|h| !h.is_portable())
        .map(|h| Skipped { name: h.event.as_str(), why: NON_PORTABLE })
        .collect()
}

/// "mcp server registered": every portable server is present under `key_path`.
/// `where_` is the "not <where>" phrase for the failure (e.g. `"not in mcp.json"`,
/// `"not under `mcp` in opencode.json"`) and `fix` its remediation, so each harness
/// keeps its exact wording. Never a failure for an entry that was never written:
/// those are the `Warn` [`registered_status`] renders.
pub(crate) fn check_mcp_registered(
    servers: &[McpServer],
    root: Option<&Value>,
    key_path: &[&str],
    where_: &str,
    fix: &str,
) -> DoctorCheck {
    let name = "mcp server registered";
    let portable: Vec<&str> = servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect();
    let skipped = skipped_mcp(servers, &portable);
    if portable.is_empty() {
        return note_skipped(DoctorCheck { name, status: CheckStatus::Ok(NO_MCP.into()) }, &skipped);
    }
    let obj = navigate(root, key_path);
    let missing: Vec<&str> = portable.iter().copied().filter(|n| obj.is_none_or(|o| !o.contains_key(*n))).collect();
    let check = if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} registered", portable.join(", "))) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp server(s) {where_}: {}", missing.join(", ")),
                fix: fix.into(),
            },
        }
    };
    note_skipped(check, &skipped)
}

/// The shared "this plugin declares no mcp server" wording.
pub(crate) const NO_MCP: &str = "no portable mcp servers to register";

/// "mcp command on PATH": every portable stdio server's command resolves. Only a
/// bare executable name is a PATH lookup; a path/variable command can't be checked
/// generically and is skipped.
pub(crate) fn check_mcp_command(servers: &[McpServer]) -> DoctorCheck {
    let name = "mcp command on PATH";
    let missing: Vec<String> = servers
        .iter()
        .filter(|s| s.is_portable() && matches!(s.kind, McpKind::Stdio))
        .map(|s| s.command.clone())
        .filter(|c| !c.is_empty() && !c.contains('/') && !c.contains('\\') && !c.contains('$') && which::which(c).is_err())
        .collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok("all referenced mcp commands resolve".into()) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp command(s) not on PATH: {}", missing.join(", ")),
                fix: "install the missing binaries into a PATH directory".into(),
            },
        }
    }
}

/// Walk `key_path` into `root`, returning the object at the leaf (`None` if any
/// level is absent or not an object). Reproduces a backend's `.get(a).get(b)…` chain.
fn navigate<'a>(root: Option<&'a Value>, key_path: &[&str]) -> Option<&'a Map<String, Value>> {
    let mut cur = root?;
    for key in key_path {
        cur = cur.get(key)?;
    }
    cur.as_object()
}

// --- per-surface probe composition -------------------------------------------
//
// A backend widens its `probe` by classifying each surface it writes (mcp, hooks,
// commands, agents) into an `Option<BackendState>` — `None` meaning it owns nothing
// for that surface for this plugin — then folding the `Some`s through `compose`.
// This keeps the marker×state table in `self_heal` unchanged while letting a broken
// hook/command tree behind a healthy mcp entry read as `NeedsRepair` (and a partial
// deletion never collapse the whole backend to `Absent`, orphaning live surfaces).

/// Fold each surface a backend writes into the single [`BackendState`] self_heal
/// keys on. Precedence, total and in this order: no surface contributed -> `Healthy`
/// (the backend owns nothing for this plugin, so its marker is kept); any `Disabled`
/// -> `Disabled` (a deliberate disable freezes the backend, never re-enabled); all
/// `Absent` -> `Absent` (the whole plugin is gone, so never resurrect); any
/// `NeedsRepair` OR a partial `Absent` (some surfaces gone, some present) ->
/// `NeedsRepair` (drift; reconcile re-adds); else `Healthy`.
pub(crate) fn compose(states: impl IntoIterator<Item = BackendState>) -> BackendState {
    let mut any = false;
    let mut disabled = false;
    let mut absent = false;
    let mut present = false; // any non-Absent surface (Healthy or NeedsRepair)
    let mut needs_repair = false;
    for state in states {
        any = true;
        match state {
            BackendState::Disabled => disabled = true,
            BackendState::Absent => absent = true,
            BackendState::NeedsRepair => {
                needs_repair = true;
                present = true;
            }
            BackendState::Healthy => present = true,
        }
    }
    if !any {
        return BackendState::Healthy;
    }
    if disabled {
        return BackendState::Disabled;
    }
    if absent && !present {
        return BackendState::Absent;
    }
    // `absent` alone now means a *partial* Absent (the all-Absent case returned above).
    if needs_repair || absent {
        return BackendState::NeedsRepair;
    }
    BackendState::Healthy
}

/// Classify a set of files a backend WOULD write (each `(path, rendered bytes)`),
/// comparing on-disk bytes to the render. `is_ours` decides, for a file present but
/// not byte-matching, whether it is our drifted file (counts as drift) or a foreign
/// same-named file to leave alone (contributes nothing); untagged surfaces pass
/// `|_, _| true`. All present+matching -> `Healthy`; all missing -> `Absent`; a mix,
/// or any drifted file of ours -> `NeedsRepair`; nothing of ours to write (empty
/// input, or every present file is foreign) -> `None`.
pub(crate) fn probe_files(expected: &[(PathBuf, Vec<u8>)], is_ours: impl Fn(&Path, &[u8]) -> bool) -> Result<Option<BackendState>> {
    let (mut considered, mut matched, mut mismatched, mut missing) = (0usize, 0usize, 0usize, 0usize);
    for (path, want) in expected {
        match fs::read(path) {
            Ok(existing) if existing == *want => {
                considered += 1;
                matched += 1;
            }
            Ok(existing) => {
                if is_ours(path, &existing) {
                    considered += 1;
                    mismatched += 1;
                }
                // else: a foreign same-named file — not ours, contributes nothing.
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                considered += 1;
                missing += 1;
            }
            Err(source) => return Err(Error::Io { context: format!("reading {}", path.display()), source }),
        }
    }
    if considered == 0 {
        return Ok(None);
    }
    Ok(Some(if mismatched > 0 || (missing > 0 && matched > 0) {
        BackendState::NeedsRepair
    } else if missing == considered {
        BackendState::Absent
    } else {
        BackendState::Healthy
    }))
}

/// Classify an owned JSON subtree (a key the backend rewrites whole, e.g. antigravity
/// hooks under `<plugin>`). `rendered` is `None` when the backend writes nothing for
/// this surface (-> `None`). Key absent -> `Absent`; deep-equal -> `Healthy`; present
/// but different -> `NeedsRepair`.
pub(crate) fn probe_json_subtree(path: &Path, key_path: &[&str], rendered: Option<Value>) -> Result<Option<BackendState>> {
    let Some(rendered) = rendered else {
        return Ok(None);
    };
    let root = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|e| Error::Config { path: path.display().to_string(), detail: e.to_string() })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some(BackendState::Absent)),
        Err(source) => return Err(Error::Io { context: format!("reading {}", path.display()), source }),
    };
    Ok(Some(match value_at(&root, key_path) {
        None => BackendState::Absent,
        Some(existing) if *existing == rendered => BackendState::Healthy,
        Some(_) => BackendState::NeedsRepair,
    }))
}

/// Classify an add-if-absent JSON surface (our rendered entries merged into arrays we
/// share with the user, e.g. gemini/cursor/codex hooks). Each `(array key_path,
/// entry)` is one entry the backend ensures present; the ownership filter must mirror
/// reconcile's writable set exactly. All present -> `Healthy`; none -> `Absent`; some
/// -> `NeedsRepair`; nothing to write -> `None`.
pub(crate) fn probe_json_entries(path: &Path, entries: &[(Vec<String>, Value)]) -> Result<Option<BackendState>> {
    if entries.is_empty() {
        return Ok(None);
    }
    let root = match fs::read(path) {
        Ok(bytes) => Some(
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|e| Error::Config { path: path.display().to_string(), detail: e.to_string() })?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => return Err(Error::Io { context: format!("reading {}", path.display()), source }),
    };
    let mut present = 0usize;
    for (key_path, entry) in entries {
        let keys: Vec<&str> = key_path.iter().map(String::as_str).collect();
        if root.as_ref().and_then(|r| array_at(r, &keys)).is_some_and(|arr| arr.iter().any(|e| e == entry)) {
            present += 1;
        }
    }
    Ok(Some(if present == 0 {
        BackendState::Absent
    } else if present == entries.len() {
        BackendState::Healthy
    } else {
        BackendState::NeedsRepair
    }))
}

/// Walk `key_path` into `root`, returning the value at the leaf (no object/array
/// coercion). `[]` returns `root` itself.
fn value_at<'a>(root: &'a Value, key_path: &[&str]) -> Option<&'a Value> {
    let mut cur = root;
    for key in key_path {
        cur = cur.get(key)?;
    }
    Some(cur)
}

fn array_at<'a>(root: &'a Value, key_path: &[&str]) -> Option<&'a Vec<Value>> {
    value_at(root, key_path)?.as_array()
}

#[cfg(test)]
#[path = "../../tests/unit/report.rs"]
mod report_tests;
