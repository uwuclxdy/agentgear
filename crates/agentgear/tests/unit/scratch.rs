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

/// The fixture trees outside this crate — `crates/host-fixture/tests/`,
/// `examples/*/tests/`, and the library crate's own top-level `tests/*.rs`
/// integration crates — cannot call `crate::scratch::path` (it is
/// crate-private), so every integration file builds its temp root in place,
/// stamped by interpolating the pid into the name. The pin binds those trees
/// with a counting standard applied to non-comment lines only: several helpers
/// describe the stamp in their doc comment, and prose counting as a stamp buys
/// exactly the slack a new pid-free root needs. After the strip, a file naming
/// `temp_dir` must carry at least as many fully-spelled `std::process::id`
/// calls as `temp_dir` uses; a new pid-free root breaks that bound, an
/// already-stamped file keeps it. What a line count cannot see — none of it
/// live in the trees today (measured): the literal inside a block comment, a
/// trailing `//` comment, or a string; a root stamped through a
/// `use std::process;` import; and a root built without the `temp_dir` token.
/// Each reads as a missing stamp and the sweep reds until the file spells
/// itself out. The sweep reads sibling-crate sources by path, so a standalone
/// packaged build of this crate (which ships `tests/unit` but not its
/// siblings) reds loudly at the first missing root rather than skipping.
#[test]
fn no_fixture_test_builds_a_pid_free_temp_path() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let crates = manifest.parent().unwrap_or_else(|| panic!("CARGO_MANIFEST_DIR has no parent"));
    let repo = crates.parent().unwrap_or_else(|| panic!("the crates dir has no parent"));

    let mut roots: Vec<(String, PathBuf)> = vec![("host-fixture tests".into(), crates.join("host-fixture/tests"))];
    let examples = repo.join("examples");
    for entry in std::fs::read_dir(&examples).unwrap_or_else(|e| panic!("reading the examples dir {}: {e}", examples.display())) {
        let path = entry.unwrap_or_else(|e| panic!("reading an entry of {}: {e}", examples.display())).path();
        let tests = path.join("tests");
        if path.is_dir() && tests.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            roots.push((format!("examples/{name}"), tests));
        }
    }
    roots.push(("agentgear tests".into(), manifest.join("tests")));

    let mut total_files = 0usize;
    let mut with_temp = 0usize;
    let mut offenders = Vec::new();
    for (label, root) in &roots {
        let mut files = Vec::new();
        collect_rs(root, &mut files);
        if label == "agentgear tests" {
            // `tests/unit/` is the unit pin's tree: its helper and its blessed
            // read-only list speak a "no `temp_dir` at all" predicate, so the
            // files this sweep wants are the top-level integration crates only.
            let unit = root.join("unit");
            files.retain(|p| !p.starts_with(&unit));
        }
        assert!(
            !files.is_empty(),
            "the sweep found no test files under {label} ({}), so that root's clean result proves nothing",
            root.display()
        );
        let mut root_temp = 0usize;
        for file in &files {
            let body = std::fs::read_to_string(file)
                .unwrap_or_else(|e| panic!("reading fixture test {} for the temp-dir sweep: {e}", file.display()));
            let (temp, pids) = count_temp_dir_uses(&body);
            if temp == 0 {
                continue;
            }
            root_temp += 1;
            if temp > pids {
                offenders.push(format!("{file:?}: {temp} `temp_dir` uses against {pids} `std::process::id` stamps"));
            }
        }
        total_files += files.len();
        with_temp += root_temp;
    }

    assert!(
        total_files > 25 && with_temp > 25,
        "the sweep walked {total_files} files ({with_temp} naming `temp_dir`) under the fixture trees, so a clean result proves nothing"
    );
    assert!(
        offenders.is_empty(),
        "every fixture test temp root carries the pid (`std::process::id()`) that a fixed name or a clock-seeded \
         draw cannot supply: either collides with a concurrent test process, one then taking the other's tree \
         mid-write. {offenders:?}"
    );
}

/// The per-file count behind the fixture pin, extracted so the counting itself
/// carries a positive control: a synthetic input the scan must flag beside ones
/// it must not, so a stub comparison cannot keep the sweep green.
fn count_temp_dir_uses(body: &str) -> (usize, usize) {
    let (mut temp, mut pids) = (0usize, 0usize);
    for line in body.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        temp += line.matches("temp_dir").count();
        pids += line.matches("std::process::id").count();
    }
    (temp, pids)
}

#[test]
fn count_temp_dir_uses_flags_a_pid_free_root() {
    let stamped = "let root = std::env::temp_dir().join(format!(\"ez-x-{}\", std::process::id()));\n";
    assert_eq!(count_temp_dir_uses(stamped), (1, 1), "a stamped root is the must-not-flag control");

    let pid_free = "let root = std::env::temp_dir().join(\"ez-fixed\");\n";
    assert_eq!(count_temp_dir_uses(pid_free), (1, 0), "the must-flag input must count as an offender");

    let documented = format!("/// `name` must be unique per test: `std::process::id()` alone is constant.\n{stamped}");
    assert_eq!(count_temp_dir_uses(&documented), (1, 1), "the doc-comment mention must not buy slack");
    let documented_with_second_root = format!("{documented}{pid_free}");
    let (temp, pids) = count_temp_dir_uses(&documented_with_second_root);
    assert!(temp > pids, "a pid-free root beside a documented stamp must break the bound: {temp} vs {pids}");
}

/// Recursive, so a future subdirectory under any swept root cannot sit in a
/// blind spot that the sweep's own anti-zero guard would still count as covered.
fn collect_rs(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("reading sweep dir {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.unwrap_or_else(|e| panic!("reading an entry of {}: {e}", dir.display())).path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
