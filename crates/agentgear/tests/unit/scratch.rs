//! The one place a unit test gets a scratch filesystem path.
//!
//! `fastrand` seeds its thread-local rng from `DefaultHasher(Instant::now(),
//! ThreadId)`, and `ThreadId` is a per-process counter, so the only cross-process
//! entropy is the clock. Two nextest processes (one test each) that start inside
//! the same tick therefore draw the *same* u64 and land on the *same* directory —
//! measured at ~1 collision per 96k processes, which is a red every few hundred
//! full-suite runs when one sibling's `remove_dir_all` takes the other's tree
//! mid-write. The pid is the entropy `fastrand` cannot supply.

use std::path::PathBuf;

/// A temp path unique to this process and to this call, named after `prefix`.
pub(crate) fn path(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{prefix}-{}-{:016x}", std::process::id(), fastrand::u64(..)))
}

#[test]
fn scratch_path_carries_this_process_id() {
    let rendered = path("ez-scratch-pin");
    let name = rendered.file_name().unwrap().to_string_lossy().into_owned();

    assert!(
        name.contains(&std::process::id().to_string()),
        "a scratch name without the pid collides with a sibling test process that drew the same fastrand seed: {name}"
    );
    assert!(name.starts_with("ez-scratch-pin-"), "prefix must survive: {name}");
    assert_ne!(path("ez-scratch-pin"), rendered, "two calls must not share a path");
}

/// The failure mode this fix has is a *new* unit file copying the old
/// `temp_dir().join(format!("…{:016x}", fastrand::u64(..)))` shape, which the
/// single-helper pin above cannot see. So pin the tree-wide property instead:
/// randomness in a unit test's path comes from this module or from nowhere.
#[test]
fn no_unit_test_draws_its_own_path_randomness() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/unit");
    let mut offenders = Vec::new();
    let mut scanned = 0usize;

    for entry in std::fs::read_dir(&dir).unwrap() {
        let file = entry.unwrap().path();
        if file.extension().is_none_or(|e| e != "rs") || file.file_name().is_some_and(|n| n == "scratch.rs") {
            continue;
        }
        scanned += 1;
        if std::fs::read_to_string(&file).unwrap().contains("fastrand") {
            offenders.push(file.file_name().unwrap().to_string_lossy().into_owned());
        }
    }

    assert!(scanned > 30, "the scan found only {scanned} unit files, so a clean result proves nothing");
    assert!(
        offenders.is_empty(),
        "these unit files draw their own path randomness instead of calling `crate::scratch::path`, \
         so two test processes seeded alike share a directory: {offenders:?}"
    );
}
