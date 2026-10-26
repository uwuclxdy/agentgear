//! `agentsmd` unit tests: the tagged-block reconcile/remove/probe cycle against
//! a scratch AGENTS.md. Pins the exact rendered bytes (create, append with the
//! separator), true-NoOp convergence, drift repair that leaves outside bytes
//! byte-identical, ownership scoped to our marker id (a rival block and user
//! lines survive every op), CRLF user content passing through verbatim around
//! our LF block, and the `None`-instructions no-op.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{guidance_check, probe, reconcile, remove};
use crate::agents::BackendState;
use crate::doctor::CheckStatus;
use crate::host::Plugin;

const ID: &str = "ez-plugin@ez-mkt";
const RIVAL_ID: &str = "rival-plugin@rival-mkt";
/// Host guidance WITHOUT a trailing newline, so every expected block below pins
/// the newline the render guarantees.
const GUIDANCE: &str = "always-loaded line.";

fn plugin(instructions: Option<&str>) -> Plugin {
    Plugin {
        name: "ez-plugin",
        marketplace: "ez-mkt",
        version: "0.0.0",
        agents: &[],
        instructions: instructions.map(str::to_string),
        blob: &[],
    }
}

fn block(id: &str) -> Vec<u8> {
    format!("<!-- agentgear:{id} begin -->\n{GUIDANCE}\n<!-- agentgear:{id} end -->\n").into_bytes()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = crate::scratch::path("ez-agmd-unit");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn reconcile_creates_the_block_on_an_absent_file() {
    let path = scratch("create-AGENTS.md");
    assert!(reconcile(&path, &plugin(Some(GUIDANCE))).unwrap(), "first reconcile must report a change");
    assert_eq!(std::fs::read(&path).unwrap(), block(ID), "a created file holds exactly our block, no leading blank line");
}

#[test]
fn reconcile_appends_the_block_after_user_content_with_one_separator() {
    // Terminated user file: existing bytes + one blank line + block.
    let terminated = scratch("append-term-AGENTS.md");
    std::fs::write(&terminated, b"user rule\n").unwrap();
    assert!(reconcile(&terminated, &plugin(Some(GUIDANCE))).unwrap());
    let mut want = b"user rule\n\n".to_vec();
    want.extend_from_slice(&block(ID));
    assert_eq!(std::fs::read(&terminated).unwrap(), want, "append must keep the user bytes and add exactly one blank line");

    // Unterminated user file: exactly one newline terminates their last line,
    // then the separator blank line, then the block.
    let bare = scratch("append-bare-AGENTS.md");
    std::fs::write(&bare, b"user rule").unwrap();
    assert!(reconcile(&bare, &plugin(Some(GUIDANCE))).unwrap());
    let mut want = b"user rule\n\n".to_vec();
    want.extend_from_slice(&block(ID));
    assert_eq!(std::fs::read(&bare).unwrap(), want, "an unterminated file gains exactly the terminator + separator");
}

#[test]
fn second_reconcile_over_a_converged_block_writes_nothing() {
    let path = scratch("idem-AGENTS.md");
    std::fs::write(&path, b"user rule\n").unwrap();
    assert!(reconcile(&path, &plugin(Some(GUIDANCE))).unwrap());
    let once = std::fs::read(&path).unwrap();
    assert!(!reconcile(&path, &plugin(Some(GUIDANCE))).unwrap(), "a converged block must report no change");
    assert_eq!(std::fs::read(&path).unwrap(), once, "a no-op reconcile must not rewrite the file");
}

#[test]
fn reconcile_repairs_drift_between_the_markers_leaving_outside_bytes_identical() {
    let path = scratch("drift-AGENTS.md");
    let seed = format!("keep left\n<!-- agentgear:{ID} begin -->\ntampered\n<!-- agentgear:{ID} end -->\nkeep right\n");
    std::fs::write(&path, seed.as_bytes()).unwrap();
    assert!(reconcile(&path, &plugin(Some(GUIDANCE))).unwrap(), "drift must report a change");
    let mut want = b"keep left\n".to_vec();
    want.extend_from_slice(&block(ID));
    want.extend_from_slice(b"keep right\n");
    assert_eq!(std::fs::read(&path).unwrap(), want, "repair replaces only the span between the markers");
}

#[test]
fn remove_strips_the_block_and_separator_keeping_user_bytes_exact() {
    let path = scratch("remove-user-AGENTS.md");
    std::fs::write(&path, b"user rule\n").unwrap();
    assert!(reconcile(&path, &plugin(Some(GUIDANCE))).unwrap());
    assert!(remove(&path, &plugin(Some(GUIDANCE))).unwrap(), "remove over a present block must report a change");
    assert_eq!(std::fs::read(&path).unwrap(), b"user rule\n", "remove must restore the user's bytes exactly");
}

#[test]
fn remove_of_a_file_holding_only_our_block_takes_the_file() {
    let path = scratch("remove-owned-AGENTS.md");
    std::fs::write(&path, block(ID)).unwrap();
    assert!(remove(&path, &plugin(Some(GUIDANCE))).unwrap());
    assert!(!path.exists(), "our own removal emptied the file, so the file goes too");
}

#[test]
fn a_rival_id_block_survives_reconcile_and_remove_byte_identical() {
    let path = scratch("rival-AGENTS.md");
    let rival = format!("user line\n<!-- agentgear:{RIVAL_ID} begin -->\nrival guidance\n<!-- agentgear:{RIVAL_ID} end -->\n");
    std::fs::write(&path, rival.as_bytes()).unwrap();

    assert!(reconcile(&path, &plugin(Some(GUIDANCE))).unwrap());
    let mut appended = rival.clone().into_bytes();
    appended.push(b'\n');
    appended.extend_from_slice(&block(ID));
    assert_eq!(std::fs::read(&path).unwrap(), appended, "reconcile must append ours, leaving the rival block untouched");

    assert!(remove(&path, &plugin(Some(GUIDANCE))).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), rival.into_bytes(), "remove must strip only our block, keeping the rival's and the user's");
}

#[test]
fn crlf_user_content_survives_verbatim_around_our_lf_block() {
    let path = scratch("crlf-AGENTS.md");
    let seed = &b"line one\r\nline two\r\n"[..];
    std::fs::write(&path, seed).unwrap();
    assert!(reconcile(&path, &plugin(Some(GUIDANCE))).unwrap());
    let installed = std::fs::read(&path).unwrap();
    let mut want = seed.to_vec();
    want.push(b'\n');
    want.extend_from_slice(&block(ID));
    assert_eq!(installed, want, "CRLF user bytes must pass through untouched around the LF separator + block");
    assert!(remove(&path, &plugin(Some(GUIDANCE))).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), seed, "remove must restore the CRLF bytes exactly");
}

#[test]
fn a_plugin_without_instructions_owns_no_surface() {
    let path = scratch("none-AGENTS.md");
    assert!(probe(&path, &plugin(None)).unwrap().is_none(), "no declared guidance contributes no probe entry");
    assert!(!reconcile(&path, &plugin(None)).unwrap());
    assert!(!path.exists(), "a guidance-less reconcile must not even create the file");
    assert!(!remove(&path, &plugin(None)).unwrap());

    std::fs::write(&path, b"user notes\n").unwrap();
    assert!(!reconcile(&path, &plugin(None)).unwrap());
    assert!(!remove(&path, &plugin(None)).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"user notes\n", "a guidance-less plugin must never touch user bytes");
}

#[test]
fn withdrawing_the_guidance_reconciles_the_block_away() {
    let path = scratch("withdraw-AGENTS.md");
    std::fs::write(&path, b"user notes\n").unwrap();
    assert!(reconcile(&path, &plugin(Some(GUIDANCE))).unwrap());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("user notes\n\n{}", String::from_utf8(block(ID)).unwrap()),
        "precondition: the block landed after the user's content"
    );

    // The host ships a version that declares no guidance: the next reconcile must take
    // the block back out rather than strand the previous text in an always-loaded file.
    assert!(reconcile(&path, &plugin(None)).unwrap(), "withdrawing the guidance must strip the block");
    assert_eq!(std::fs::read(&path).unwrap(), b"user notes\n", "the user's bytes survive the withdrawal byte-identical");
}

#[test]
fn a_stranded_block_reads_needs_repair_when_the_guidance_is_withdrawn() {
    let path = scratch("stranded-AGENTS.md");
    std::fs::write(&path, block(ID)).unwrap();
    assert!(
        matches!(probe(&path, &plugin(None)).unwrap(), Some(BackendState::NeedsRepair)),
        "a block left by withdrawn guidance is stale state a heal must clear"
    );
    assert!(remove(&path, &plugin(None)).unwrap(), "the markers, not the declared text, decide what remove takes");
    assert!(!path.exists(), "removing the only content takes the file our own removal emptied");
}

#[test]
fn a_file_without_our_markers_probes_absent_and_removes_nothing() {
    let path = scratch("absent-AGENTS.md");
    std::fs::write(&path, b"just user notes\n").unwrap();
    assert!(
        matches!(probe(&path, &plugin(Some(GUIDANCE))).unwrap(), Some(BackendState::Absent)),
        "a hand-deleted block must read Absent so self-heal repairs it"
    );
    assert!(!remove(&path, &plugin(Some(GUIDANCE))).unwrap(), "remove over a marker-less file is a no-op");
    assert_eq!(std::fs::read(&path).unwrap(), b"just user notes\n", "a marker-less remove must not touch the file");
}

#[test]
fn probe_reads_healthy_and_needs_repair_directly() {
    let path = scratch("states-AGENTS.md");
    std::fs::write(&path, block(ID)).unwrap();
    assert!(matches!(probe(&path, &plugin(Some(GUIDANCE))).unwrap(), Some(BackendState::Healthy)), "an exact render must read Healthy");
    std::fs::write(&path, format!("<!-- agentgear:{ID} begin -->\nedited\n<!-- agentgear:{ID} end -->\n")).unwrap();
    assert!(
        matches!(probe(&path, &plugin(Some(GUIDANCE))).unwrap(), Some(BackendState::NeedsRepair)),
        "drifted marker content must read NeedsRepair"
    );
}

#[test]
fn a_whitespace_only_file_is_treated_as_absent() {
    let path = scratch("blank-AGENTS.md");
    std::fs::write(&path, b"  \n\t\n").unwrap();
    assert!(reconcile(&path, &plugin(Some(GUIDANCE))).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), block(ID), "there is no user content to preserve, so the file becomes exactly our block");
    // and our removal of it still takes the file: the remainder is nothing.
    assert!(remove(&path, &plugin(Some(GUIDANCE))).unwrap());
    assert!(!path.exists(), "whitespace we overwrote is not user content to restore");
}

#[test]
fn remove_keeps_a_user_terminator_when_no_separator_sits_above_a_hand_placed_block() {
    // The user pasted our markers directly under their line, with no blank line:
    // remove strips the block but must NOT eat their line's terminator.
    let path = scratch("bare-term-AGENTS.md");
    let mut seed = b"user line\n".to_vec();
    seed.extend_from_slice(&block(ID));
    std::fs::write(&path, &seed).unwrap();
    assert!(remove(&path, &plugin(Some(GUIDANCE))).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"user line\n", "no separator was ours to strip, the terminator survives");
}

#[test]
fn marker_lines_with_crlf_terminators_round_trip() {
    // Our block hand-saved with CRLF line endings: the span consumes the `\r\n`
    // after the end marker, so repair and remove both leave the user bytes exact.
    let path = scratch("crlf-span-AGENTS.md");
    let block_crlf = format!("<!-- agentgear:{ID} begin -->\r\n{GUIDANCE}\r\n<!-- agentgear:{ID} end -->\r\n");
    std::fs::write(&path, format!("user line\n{block_crlf}tail\r\n")).unwrap();
    assert!(reconcile(&path, &plugin(Some(GUIDANCE))).unwrap(), "CRLF marker lines must still count as our span");
    let mut want = b"user line\n".to_vec();
    want.extend_from_slice(&block(ID));
    want.extend_from_slice(b"tail\r\n");
    assert_eq!(std::fs::read(&path).unwrap(), want, "repair re-renders the span in our canonical LF form, outside bytes untouched");
    assert!(remove(&path, &plugin(Some(GUIDANCE))).unwrap());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"user line\ntail\r\n",
        "remove strips the CRLF span and its separator, keeping both user lines"
    );
}

#[test]
fn a_block_at_eof_without_a_final_terminator_is_still_our_span() {
    let path = scratch("eof-AGENTS.md");
    let mut seed = b"user line\n".to_vec();
    seed.extend_from_slice(&block(ID));
    seed.pop(); // end marker at EOF, no trailing newline
    std::fs::write(&path, &seed).unwrap();
    assert!(reconcile(&path, &plugin(Some(GUIDANCE))).unwrap(), "the terminator-less span must still be found and re-rendered");
    let mut want = b"user line\n".to_vec();
    want.extend_from_slice(&block(ID));
    assert_eq!(std::fs::read(&path).unwrap(), want, "the re-render restores the canonical terminator");
    std::fs::write(&path, &seed).unwrap(); // back to the terminator-less span, then remove
    assert!(remove(&path, &plugin(Some(GUIDANCE))).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"user line\n", "remove over the terminator-less span keeps the user bytes");
}

#[test]
fn only_the_first_span_is_ours_a_hand_duplicated_second_block_survives() {
    let path = scratch("dupe-AGENTS.md");
    let mut seed = b"user line\n".to_vec();
    seed.extend_from_slice(&block(ID));
    seed.extend_from_slice(&block(ID));
    std::fs::write(&path, &seed).unwrap();
    assert!(
        matches!(probe(&path, &plugin(Some(GUIDANCE))).unwrap(), Some(BackendState::Healthy)),
        "the first span matches, so the surface reads Healthy"
    );
    assert!(
        !reconcile(&path, &plugin(Some(GUIDANCE))).unwrap(),
        "a converged first span means no write, the duplicate is not ours to touch"
    );
    assert!(remove(&path, &plugin(Some(GUIDANCE))).unwrap());
    let mut want = b"user line\n".to_vec();
    want.extend_from_slice(&block(ID));
    assert_eq!(std::fs::read(&path).unwrap(), want, "remove strips the first copy; the user's hand copy stays");
}

#[test]
fn a_begin_marker_without_its_end_is_user_content() {
    let path = scratch("orphan-begin-AGENTS.md");
    let seed = format!("user line\n<!-- agentgear:{ID} begin -->\n");
    std::fs::write(&path, seed.as_bytes()).unwrap();
    assert!(
        matches!(probe(&path, &plugin(Some(GUIDANCE))).unwrap(), Some(BackendState::Absent)),
        "an unterminated begin is no block of ours"
    );
    assert!(reconcile(&path, &plugin(Some(GUIDANCE))).unwrap());
    let mut want = seed.into_bytes();
    want.push(b'\n');
    want.extend_from_slice(&block(ID));
    assert_eq!(std::fs::read(&path).unwrap(), want, "the orphan line survives as user content and our block appends after it");
}

#[test]
fn non_utf8_user_bytes_round_trip() {
    let path = scratch("binary-AGENTS.md");
    let seed = &b"user \xff\xfe bytes\n"[..];
    std::fs::write(&path, seed).unwrap();
    assert!(reconcile(&path, &plugin(Some(GUIDANCE))).unwrap());
    let mut want = seed.to_vec();
    want.push(b'\n');
    want.extend_from_slice(&block(ID));
    assert_eq!(std::fs::read(&path).unwrap(), want, "raw non-UTF-8 bytes pass through the append untouched");
    assert!(remove(&path, &plugin(Some(GUIDANCE))).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), seed, "remove restores the exact bytes");
}

#[test]
fn guidance_check_classifies_by_probe_not_file_existence() {
    let path = scratch("doctor-AGENTS.md");

    // A scope with no surface (project scope) reads as a warn, never a fail.
    let no_surface = guidance_check(Ok(None), &plugin(Some(GUIDANCE)));
    assert!(matches!(no_surface.status, CheckStatus::Warn(_)), "no AGENTS.md surface at this scope must warn, not fail");

    // Present file without our block: the file exists, the block does not — a fail.
    std::fs::write(&path, b"user notes only\n").unwrap();
    let missing = guidance_check(Ok(Some(path.clone())), &plugin(Some(GUIDANCE)));
    assert!(matches!(missing.status, CheckStatus::Fail { .. }), "a file without our block must fail the check");

    // Healthy block: ok.
    reconcile(&path, &plugin(Some(GUIDANCE))).unwrap();
    let healthy = guidance_check(Ok(Some(path)), &plugin(Some(GUIDANCE)));
    assert!(matches!(healthy.status, CheckStatus::Ok(_)), "a converged block must read ok");
}
