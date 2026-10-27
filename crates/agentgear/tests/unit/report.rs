//! `report::compose` unit tests: the total precedence the per-surface probe
//! composition rests on. Every row of the precedence table (incl. the mixed cases)
//! is pinned here so a rule reorder reds immediately. Plus `note_skipped` /
//! `check_mcp_registered`: a dropped entry is always named, and a server the
//! backend's shape never writes is never demanded.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{NO_MCP, compose, note_skipped, skipped_hooks, skipped_mcp};
use crate::agents::BackendState::{self, Absent, Disabled, Healthy, NeedsRepair};
use crate::components::{HookBinding, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck};

/// `compose` returns one of four states; name them for exact assertions without
/// needing `BackendState: PartialEq` in production.
fn tag(state: BackendState) -> &'static str {
    match state {
        Absent => "absent",
        Healthy => "healthy",
        Disabled => "disabled",
        NeedsRepair => "needs_repair",
    }
}

fn composed(states: impl IntoIterator<Item = BackendState>) -> &'static str {
    tag(compose(states))
}

#[test]
fn empty_is_healthy_so_a_surfaceless_backend_keeps_its_marker() {
    // No surface contributed (the mcp-less/hook-less plugin) must never read Absent —
    // that would drop a present marker in self_heal.
    assert_eq!(composed([]), "healthy");
}

#[test]
fn single_surface_is_identity() {
    // compose([Some(mcp)]) == mcp keeps every mcp-only path byte-for-byte unchanged.
    assert_eq!(composed([Healthy]), "healthy");
    assert_eq!(composed([Absent]), "absent");
    assert_eq!(composed([Disabled]), "disabled");
    assert_eq!(composed([NeedsRepair]), "needs_repair");
}

#[test]
fn any_disabled_freezes_the_backend() {
    // A deliberate disable outranks everything else, so self_heal no-ops (never re-enables).
    assert_eq!(composed([Disabled, Healthy]), "disabled");
    assert_eq!(composed([Healthy, Disabled]), "disabled");
    assert_eq!(composed([Disabled, NeedsRepair]), "disabled");
    assert_eq!(composed([Disabled, Absent]), "disabled");
}

#[test]
fn all_absent_is_absent_so_a_gone_plugin_is_never_resurrected() {
    assert_eq!(composed([Absent, Absent]), "absent");
    assert_eq!(composed([Absent, Absent, Absent]), "absent");
}

#[test]
fn partial_absent_is_needs_repair() {
    // Some surfaces gone, some present -> a partial deletion the reconcile re-adds,
    // NOT a whole-backend Absent that would orphan the surviving surface.
    assert_eq!(composed([Healthy, Absent]), "needs_repair");
    assert_eq!(composed([Absent, Healthy]), "needs_repair");
    assert_eq!(composed([Healthy, Healthy, Absent]), "needs_repair");
}

#[test]
fn any_needs_repair_wins_over_healthy() {
    // The headline bug: a broken hook surface behind a healthy mcp surface must
    // surface as NeedsRepair, not Healthy.
    assert_eq!(composed([NeedsRepair, Healthy]), "needs_repair");
    assert_eq!(composed([Healthy, NeedsRepair]), "needs_repair");
}

#[test]
fn all_healthy_is_healthy() {
    assert_eq!(composed([Healthy, Healthy, Healthy]), "healthy");
}

// --- note_skipped -------------------------------------------------------------
//
// A dropped entry is the quiet failure: the plugin installs clean and does
// nothing on that harness. These pin that every drop reaches the user, that a
// real `Fail` still outranks the note, and that the no-drop path stays
// byte-identical (it is the common one).

fn stdio(name: &str, command: &str) -> McpServer {
    McpServer { name: name.into(), kind: McpKind::Stdio, command: command.into(), args: vec![], env: Default::default() }
}

fn hook(event: &str, command: &str) -> HookBinding {
    HookBinding { event: event.into(), matcher: None, command: command.into() }
}

fn ok(detail: &str) -> DoctorCheck {
    DoctorCheck { name: "mcp server registered", status: CheckStatus::Ok(detail.into()) }
}

fn detail(check: &DoctorCheck) -> String {
    match &check.status {
        CheckStatus::Ok(d) => format!("ok: {d}"),
        CheckStatus::Warn(d) => format!("warn: {d}"),
        CheckStatus::Fail { problem, .. } => format!("fail: {problem}"),
    }
}

#[test]
fn nothing_dropped_leaves_the_check_untouched() {
    assert_eq!(detail(&note_skipped(ok("ez registered"), &[])), "ok: ez registered");
    assert_eq!(detail(&note_skipped(ok(NO_MCP), &[])), "ok: no portable mcp servers to register");
}

#[test]
fn every_entry_dropped_warns_instead_of_reading_ok() {
    // The headline case: the plugin declares servers, all of them carry the token,
    // nothing is written, and doctor used to report `Ok`.
    let servers = [stdio("ez", "${CLAUDE_PLUGIN_ROOT}/bin/ez"), stdio("fx", "${CLAUDE_PLUGIN_ROOT}/bin/fx")];
    let skipped = skipped_mcp(&servers, &[]);
    assert_eq!(
        detail(&note_skipped(ok(NO_MCP), &skipped)),
        "warn: no portable mcp servers to register; skipped ez, fx: ${CLAUDE_PLUGIN_ROOT} expands only inside Claude Code (use a bare command name)"
    );
}

#[test]
fn a_partial_drop_warns_and_still_names_what_survived() {
    let servers = [stdio("ez", "ez"), stdio("fx", "${CLAUDE_PLUGIN_ROOT}/bin/fx")];
    let skipped = skipped_mcp(&servers, &["ez"]);
    assert_eq!(
        detail(&note_skipped(ok("ez registered"), &skipped)),
        "warn: ez registered; skipped fx: ${CLAUDE_PLUGIN_ROOT} expands only inside Claude Code (use a bare command name)"
    );
}

#[test]
fn a_real_failure_outranks_the_note() {
    let servers = [stdio("ez", "ez"), stdio("fx", "${CLAUDE_PLUGIN_ROOT}/bin/fx")];
    let skipped = skipped_mcp(&servers, &["ez"]);
    let failing = DoctorCheck {
        name: "mcp server registered",
        status: CheckStatus::Fail { problem: "mcp server(s) not in mcp.json: ez".into(), fix: "run setup".into() },
    };
    assert_eq!(detail(&note_skipped(failing, &skipped)), "fail: mcp server(s) not in mcp.json: ez");
}

#[test]
fn a_portable_entry_the_backend_cannot_render_reports_the_transport_reason() {
    // goose/zed drop a remote transport they cannot host; that entry is portable,
    // so it must not be blamed on `${CLAUDE_PLUGIN_ROOT}`.
    let servers =
        [stdio("ez", "ez"), McpServer { name: "rm".into(), kind: McpKind::Sse { url: "https://x/sse".into() }, ..stdio("rm", "") }];
    let skipped = skipped_mcp(&servers, &["ez"]);
    assert_eq!(
        detail(&note_skipped(ok("ez registered"), &skipped)),
        "ok: ez registered; skipped rm: this harness cannot host that transport"
    );
}

#[test]
fn both_reasons_group_separately() {
    let servers = [
        stdio("np", "${CLAUDE_PLUGIN_ROOT}/bin/np"),
        McpServer { name: "rm".into(), kind: McpKind::Sse { url: "https://x/sse".into() }, ..stdio("rm", "") },
    ];
    let skipped = skipped_mcp(&servers, &[]);
    assert_eq!(
        detail(&note_skipped(ok(NO_MCP), &skipped)),
        "warn: no portable mcp servers to register; skipped np: ${CLAUDE_PLUGIN_ROOT} expands only inside Claude Code (use a bare command name); \
         skipped rm: this harness cannot host that transport"
    );
}

#[test]
fn only_a_non_portable_hook_is_named_never_an_unmappable_event() {
    // An event with no analog on the harness is a documented per-backend skip; only
    // the host's own `${CLAUDE_PLUGIN_ROOT}` command is its bug.
    let hooks = [hook("SessionStart", "mytool self-heal"), hook("Stop", "${CLAUDE_PLUGIN_ROOT}/bin/mytool stop")];
    let skipped = skipped_hooks(&hooks);
    assert_eq!(skipped.len(), 1, "the portable hook must not be reported as dropped");
    assert_eq!(
        detail(&note_skipped(ok("1 hook(s) present in hooks.json"), &skipped)),
        "warn: 1 hook(s) present in hooks.json; skipped Stop: ${CLAUDE_PLUGIN_ROOT} expands only inside Claude Code (use a bare command name)"
    );
}

#[test]
fn the_shared_mcp_check_wires_the_note_in() {
    // Pins the wiring, not just the helper: a backend that stopped calling
    // `note_skipped` would still pass every test above.
    let servers = [stdio("ez", "${CLAUDE_PLUGIN_ROOT}/bin/ez")];
    let check = super::check_mcp_registered(&servers, &[], None, &["mcpServers"], "not in mcp.json", "run setup");
    assert_eq!(
        detail(&check),
        "warn: no portable mcp servers to register; skipped ez: ${CLAUDE_PLUGIN_ROOT} expands only inside Claude Code (use a bare command name)"
    );
}

#[test]
fn the_shared_mcp_check_fails_only_a_missing_written_server() {
    let servers = [stdio("ez", "ez"), stdio("fx", "fx"), stdio("np", "${CLAUDE_PLUGIN_ROOT}/bin/np")];
    let writable = ["ez", "fx"];
    let partial = serde_json::json!({"mcpServers": {"ez": {}}});
    let check = super::check_mcp_registered(&servers, &writable, Some(&partial), &["mcpServers"], "not in mcp.json", "run setup");
    assert_eq!(detail(&check), "fail: mcp server(s) not in mcp.json: fx");
    let full = serde_json::json!({"mcpServers": {"ez": {}, "fx": {}}});
    let check = super::check_mcp_registered(&servers, &writable, Some(&full), &["mcpServers"], "not in mcp.json", "run setup");
    assert_eq!(
        detail(&check),
        "warn: ez, fx registered; skipped np: ${CLAUDE_PLUGIN_ROOT} expands only inside Claude Code (use a bare command name)"
    );
}

#[test]
fn a_server_the_shape_never_writes_is_named_not_demanded() {
    // The shape filters a portable sse server out, so setup never writes it; a
    // config holding every written server is a pass, with the drop named.
    let servers =
        [stdio("ez", "ez"), McpServer { name: "rm".into(), kind: McpKind::Sse { url: "https://x/sse".into() }, ..stdio("rm", "") }];
    let written = serde_json::json!({"mcpServers": {"ez": {}}});
    let check = super::check_mcp_registered(&servers, &["ez"], Some(&written), &["mcpServers"], "not in mcp.json", "run setup");
    assert_eq!(detail(&check), "ok: ez registered; skipped rm: this harness cannot host that transport");
}

#[test]
fn a_skipped_server_never_excuses_a_missing_written_one() {
    let servers =
        [stdio("ez", "ez"), McpServer { name: "rm".into(), kind: McpKind::Sse { url: "https://x/sse".into() }, ..stdio("rm", "") }];
    let check = super::check_mcp_registered(&servers, &["ez"], None, &["mcpServers"], "not in mcp.json", "run setup");
    assert_eq!(detail(&check), "fail: mcp server(s) not in mcp.json: ez");
}

#[test]
fn every_declared_server_portable_but_none_renderable_reads_no_hostable_mcp() {
    // The all-unrenderable case (a harness that hosts only sse, given an http
    // plugin): "no portable" would be false here — the server IS portable, the
    // harness just cannot host its transport.
    let servers = [McpServer { name: "rm".into(), kind: McpKind::Http { url: "https://x/mcp".into() }, ..stdio("rm", "") }];
    let check = super::check_mcp_registered(&servers, &[], None, &["mcpServers"], "not in mcp.json", "run setup");
    assert_eq!(detail(&check), "ok: no mcp servers this harness can host; skipped rm: this harness cannot host that transport");
}

#[test]
fn a_missing_written_server_pins_the_callers_problem_and_fix() {
    // vscode-copilot's exact failure wording, through the shared check: the
    // "not in mcp.json" phrase and the project-root fix hint both ship as-is.
    let servers = [stdio("ez", "ez"), stdio("fx", "fx")];
    let written = serde_json::json!({"servers": {"ez": {}}});
    let check = super::check_mcp_registered(
        &servers,
        &["ez", "fx"],
        Some(&written),
        &["servers"],
        "not in mcp.json",
        "run the host's `setup` in the project root",
    );
    let CheckStatus::Fail { problem, fix } = &check.status else {
        panic!("a missing written server must fail, got {:?}", check.status);
    };
    assert_eq!(problem, "mcp server(s) not in mcp.json: fx");
    assert_eq!(fix, "run the host's `setup` in the project root");
}
