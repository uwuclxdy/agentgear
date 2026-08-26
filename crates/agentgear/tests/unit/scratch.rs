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

/// The unit files that reach for `temp_dir` and are not building a path under it:
/// each binds it as a read-only "a directory that exists" fixture and asserts on
/// it, so neither can collide with anything. The count is part of the entry — a
/// blessed file that grows a *second* use reds rather than inheriting the pass.
const READ_ONLY_TEMP_DIR_FIXTURES: [(&str, usize); 1] = [("vscode_copilot.rs", 1)];

/// The failure mode this fix has is a *new* unit file naming its own path under
/// the temp dir, which the single-helper pin above cannot see. Banning the
/// `fastrand` shape alone would miss the worse version of it: a fixed name
/// (`temp_dir().join("x")`) collides between *every* concurrent process, not one
/// pair in 96k. So pin what the helper's whole reason for existing is — a unit
/// test reaches the temp dir through `crate::scratch::path` or not at all —
/// which holds whatever the next author draws their uniqueness from.
#[test]
fn no_unit_test_builds_its_own_temp_path() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/unit");
    let mut files = Vec::new();
    collect_rs(&dir, &mut files);

    let mut offenders = Vec::new();
    let mut blessed_seen = 0usize;

    for file in &files {
        let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if name == "scratch.rs" {
            continue;
        }
        let allowed = READ_ONLY_TEMP_DIR_FIXTURES.iter().find(|(f, _)| *f == name).map_or(0, |(_, n)| *n);
        if allowed > 0 {
            blessed_seen += 1;
        }
        let hits = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("reading unit test {} for the temp-dir sweep: {e}", file.display()))
            .matches("temp_dir")
            .count();
        if hits != allowed {
            offenders.push(format!("{name}: {hits} `temp_dir` uses, {allowed} allowed"));
        }
    }

    assert!(files.len() > 30, "the sweep walked only {} files under {}, so a clean result proves nothing", files.len(), dir.display());
    assert!(
        offenders.is_empty(),
        "every unit test must take its scratch path from `crate::scratch::path`, which stamps the pid \
         that `fastrand` cannot supply; a hand-built temp path collides with a concurrent test process. \
         Fewer uses than allowed means a stale `READ_ONLY_TEMP_DIR_FIXTURES` entry to delete. {offenders:?}"
    );
    assert_eq!(
        blessed_seen,
        READ_ONLY_TEMP_DIR_FIXTURES.len(),
        "a `READ_ONLY_TEMP_DIR_FIXTURES` entry names a file the sweep never saw: {READ_ONLY_TEMP_DIR_FIXTURES:?}"
    );
}

/// Recursive, so a future `tests/unit/<sub>/x.rs` cannot sit in a blind spot that
/// the sweep's own anti-zero guard would still count as covered.
fn collect_rs(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("reading the unit-test dir {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.unwrap_or_else(|e| panic!("reading an entry of {}: {e}", dir.display())).path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
