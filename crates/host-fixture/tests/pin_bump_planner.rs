//! Red-first gate for the pin-bump planner: its three-field pin support, and its refusal
//! to leave an old pin on any live line, moved or not.
//!
//! `antigravity-cli` pins `version=`, `url=` and `sha512=` in one Dockerfile block,
//! while the STALE summary entry can carry versions only. `pin-staleness.sh` therefore
//! hands the planner the whole triple on a `pin-staleness: BUMP:` summary line, and
//! the planner must move all three fields or refuse the log. These cases pin that
//! apply and refusal behaviour against fixture trees. The check's live emission needs
//! the vendor endpoints, so only the scheduled workflow exercises the real read.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const AGY_OLD_URL: &str = "https://storage.example/antigravity-cli/1.5.0-111/linux-x64/cli.tar.gz";
const AGY_NEW_URL: &str = "https://storage.example/antigravity-cli/1.5.2-222/linux-x64/cli.tar.gz";

fn old_sha() -> String {
    "a".repeat(128)
}

fn new_sha() -> String {
    "b".repeat(128)
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/docker/bump-stale-pins.sh")
}

struct Case {
    root: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("agentgear-pin-bump-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("docker/antigravity-cli")).unwrap();
        fs::create_dir_all(root.join("docker/droid")).unwrap();
        Self { root }
    }

    fn docker(&self) -> PathBuf {
        self.root.join("docker")
    }

    fn write(&self, rel: &str, body: &str) {
        fs::write(self.root.join(rel), body).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn run(&self, dry_run: bool) -> Output {
        self.run_with_path(dry_run, None)
    }

    fn run_with_path(&self, dry_run: bool, path_prefix: Option<&Path>) -> Output {
        let mut cmd = Command::new("bash");
        cmd.arg(script());
        if dry_run {
            cmd.arg("--dry-run");
        }
        cmd.arg(self.root.join("log")).arg(self.docker());
        if let Some(dir) = path_prefix {
            let old = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{old}", dir.display()));
        }
        cmd.output().unwrap()
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_agy(case: &Case) {
    case.write(
        "docker/antigravity-cli/Dockerfile",
        &format!("FROM scratch\nRUN bash <<'SH'\nset -euo pipefail\nversion=1.5.0\nurl={AGY_OLD_URL}\nsha512={}\nSH\n", old_sha()),
    );
}

fn write_agy_with_tails(case: &Case) {
    case.write(
        "docker/antigravity-cli/Dockerfile",
        &format!(
            "FROM scratch\nRUN bash <<'SH'\nset -euo pipefail\nversion=1.5.0  # pinned against agy\nurl={AGY_OLD_URL}  # artifact\nsha512={}  # digest\nSH\n",
            old_sha()
        ),
    );
}

fn write_droid(case: &Case) {
    case.write("docker/droid/Dockerfile", "FROM scratch\nRUN bash <<'SH'\nset -euo pipefail\nversion=1.3.0\nSH\n");
}

const HEADER: &str = "pin-staleness: checking 2 pinned docker legs in /fixture\nleg              pin          current      status\n";

fn stale_line() -> &'static str {
    "pin-staleness: STALE: antigravity-cli (1.5.0 != 1.5.2), droid (1.3.0 != 1.5.0)\n"
}

fn bump_line(version: &str, url: &str, sha: &str) -> String {
    format!("pin-staleness: BUMP: antigravity-cli {version} {url} {sha}\n")
}

fn write_log(case: &Case, body: &str) {
    case.write("log", body);
}

fn snapshot(case: &Case) -> (String, String) {
    (case.read("docker/antigravity-cli/Dockerfile"), case.read("docker/droid/Dockerfile"))
}

fn agy_dockerfile(case: &Case) -> String {
    case.docker().join("antigravity-cli/Dockerfile").display().to_string()
}

fn shim(case: &Case, name: &str, body: &str) -> PathBuf {
    let dir = case.root.join(format!("shim-{name}"));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn line_starting(text: &str, prefix: &str) -> String {
    text.lines().find(|line| line.starts_with(prefix)).unwrap_or_else(|| panic!("no {prefix:?} line")).to_string()
}

fn assert_refused(case: &Case, expected: &str) {
    let (agy_before, droid_before) = snapshot(case);
    let out = case.run(false);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "planner accepted the log");
    assert_eq!(out.status.code(), Some(1), "a refusal exits 1");
    assert!(
        err.lines().any(|line| line == expected),
        "stderr misses the refusal line; saw: {}",
        err.lines().filter(|line| line.contains("REFUSED")).collect::<Vec<_>>().join(" | ")
    );
    assert!(out.stdout.is_empty(), "a refused plan printed a plan line");
    let (agy_after, droid_after) = snapshot(case);
    assert!(agy_after == agy_before, "a refused plan rewrote the agy Dockerfile");
    assert!(droid_after == droid_before, "a refused plan rewrote the droid Dockerfile");
}

#[test]
fn applies_the_three_field_pin_bump() {
    let case = Case::new("apply");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));

    let out = case.run(false);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "planner failed; saw: {err}");

    let agy = case.read("docker/antigravity-cli/Dockerfile");
    assert_eq!(line_starting(&agy, "version="), "version=1.5.2", "version line");
    assert_eq!(line_starting(&agy, "url="), format!("url={AGY_NEW_URL}"), "url line");
    assert_eq!(line_starting(&agy, "sha512="), format!("sha512={}", new_sha()), "sha512 line");
    assert!(!agy.contains(AGY_OLD_URL), "the 1.5.0 url survived");

    let droid = case.read("docker/droid/Dockerfile");
    assert_eq!(line_starting(&droid, "version="), "version=1.5.0", "droid version line");

    let plan = String::from_utf8_lossy(&out.stdout);
    assert!(plan.lines().any(|line| line == "bump-stale-pins: PLAN: antigravity-cli 1.5.0 1.5.2"), "the agy plan line is missing");
    assert!(plan.lines().any(|line| line == "bump-stale-pins: PLAN: droid 1.3.0 1.5.0"), "the droid plan line is missing");
}

#[test]
fn refuses_a_three_field_leg_without_a_bump_payload() {
    let case = Case::new("no-payload");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}", stale_line()));
    assert_refused(
        &case,
        "bump-stale-pins: REFUSED: antigravity-cli: the log carries no BUMP payload for its three-field pin; re-run the check",
    );
}

#[test]
fn refuses_a_bump_version_that_disagrees_with_the_stale_current() {
    let case = Case::new("version-mismatch");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.3", AGY_NEW_URL, &new_sha())));
    assert_refused(&case, "bump-stale-pins: REFUSED: antigravity-cli: BUMP payload version 1.5.3 != the STALE current 1.5.2");
}

#[test]
fn refuses_a_bump_payload_for_a_leg_the_stale_summary_does_not_name() {
    let case = Case::new("orphan-payload");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}pin-staleness: STALE: droid (1.3.0 != 1.5.0)\n{}", bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    assert_refused(&case, "bump-stale-pins: REFUSED: antigravity-cli: a BUMP payload names a leg the STALE summary does not");
}

#[test]
fn refuses_a_malformed_bump_line() {
    let case = Case::new("malformed");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}pin-staleness: BUMP: antigravity-cli 1.5.2\n", stale_line()));
    assert_refused(&case, "bump-stale-pins: REFUSED: malformed BUMP line: pin-staleness: BUMP: antigravity-cli 1.5.2");
}

#[test]
fn refuses_a_duplicate_bump_payload() {
    let case = Case::new("duplicate");
    write_agy(&case);
    write_droid(&case);
    let bump = bump_line("1.5.2", AGY_NEW_URL, &new_sha());
    write_log(&case, &format!("{HEADER}{}{bump}{bump}", stale_line()));
    assert_refused(&case, "bump-stale-pins: REFUSED: antigravity-cli: named twice by the BUMP payload");
}

#[test]
fn refuses_a_non_https_bump_url() {
    let case = Case::new("http-url");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", "http://storage.example/cli.tar.gz", &new_sha())));
    assert_refused(&case, "bump-stale-pins: REFUSED: antigravity-cli: BUMP url http://storage.example/cli.tar.gz is not a plain https url");
}

#[test]
fn refuses_a_sha512_that_is_not_a_hex_digest() {
    let case = Case::new("bad-sha");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, "not-a-digest")));
    assert_refused(
        &case,
        "bump-stale-pins: REFUSED: antigravity-cli: BUMP sha512 not-a-digest is not a 128-character lowercase hex digest",
    );
}

#[test]
fn dry_run_prints_the_plan_and_writes_nothing() {
    let case = Case::new("dry-run");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    let (agy_before, droid_before) = snapshot(&case);

    let out = case.run(true);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "dry run failed; saw: {err}");
    let (agy_after, droid_after) = snapshot(&case);
    assert!(agy_after == agy_before, "a dry run rewrote the agy Dockerfile");
    assert!(droid_after == droid_before, "a dry run rewrote the droid Dockerfile");

    let plan = String::from_utf8_lossy(&out.stdout);
    assert!(plan.lines().any(|line| line == "bump-stale-pins: PLAN: antigravity-cli 1.5.0 1.5.2"), "the agy plan line is missing");
    assert!(plan.lines().any(|line| line == "bump-stale-pins: PLAN: droid 1.3.0 1.5.0"), "the droid plan line is missing");
}

#[test]
fn applies_the_three_field_pin_bump_and_keeps_line_tails() {
    let case = Case::new("tails");
    write_agy_with_tails(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));

    let out = case.run(false);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "planner failed; saw: {err}");

    let agy = case.read("docker/antigravity-cli/Dockerfile");
    assert_eq!(line_starting(&agy, "version="), "version=1.5.2  # pinned against agy", "version line tail");
    assert_eq!(line_starting(&agy, "url="), format!("url={AGY_NEW_URL}  # artifact"), "url line tail");
    assert_eq!(line_starting(&agy, "sha512="), format!("sha512={}  # digest", new_sha()), "sha512 line tail");
}

#[test]
fn refuses_a_bump_payload_for_a_single_field_leg() {
    let case = Case::new("single-field-payload");
    write_agy(&case);
    write_droid(&case);
    write_log(
        &case,
        &format!(
            "{HEADER}{}{}pin-staleness: BUMP: droid 1.5.0 {AGY_NEW_URL} {}\n",
            stale_line(),
            bump_line("1.5.2", AGY_NEW_URL, &new_sha()),
            new_sha()
        ),
    );
    assert_refused(&case, "bump-stale-pins: REFUSED: droid: a BUMP payload names a leg this planner has no three-field pin for");
}

#[test]
fn refuses_a_bump_version_that_is_not_a_version() {
    let case = Case::new("bad-bump-version");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("v1", AGY_NEW_URL, &new_sha())));
    assert_refused(&case, "bump-stale-pins: REFUSED: antigravity-cli: BUMP version v1 is not a version");
}

#[test]
fn refuses_two_url_lines_in_the_three_field_pin() {
    let case = Case::new("two-urls");
    case.write(
        "docker/antigravity-cli/Dockerfile",
        &format!("FROM scratch\nversion=1.5.0\nurl={AGY_OLD_URL}\nurl=https://storage.example/dup.tar.gz\nsha512={}\n", old_sha()),
    );
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: antigravity-cli: expected exactly one '^url=<url>' line in {}, found 2", agy_dockerfile(&case)),
    );
}

#[test]
fn refuses_a_missing_url_line_in_the_three_field_pin() {
    let case = Case::new("no-url");
    case.write("docker/antigravity-cli/Dockerfile", &format!("FROM scratch\nversion=1.5.0\nsha512={}\n", old_sha()));
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: antigravity-cli: expected exactly one '^url=<url>' line in {}, found 0", agy_dockerfile(&case)),
    );
}

#[test]
fn refuses_two_sha512_lines_in_the_three_field_pin() {
    let case = Case::new("two-shas");
    case.write(
        "docker/antigravity-cli/Dockerfile",
        &format!("FROM scratch\nversion=1.5.0\nurl={AGY_OLD_URL}\nsha512={}\nsha512={}\n", old_sha(), old_sha()),
    );
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    assert_refused(
        &case,
        &format!(
            "bump-stale-pins: REFUSED: antigravity-cli: expected exactly one '^sha512=<digest>' line in {}, found 2",
            agy_dockerfile(&case)
        ),
    );
}

#[test]
fn refuses_a_128_character_non_hex_sha512() {
    let case = Case::new("nonhex-sha");
    write_agy(&case);
    write_droid(&case);
    let sha = "z".repeat(128);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &sha)));
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: antigravity-cli: BUMP sha512 {sha} is not a 128-character lowercase hex digest"),
    );
}

#[test]
fn refuses_a_127_character_hex_sha512() {
    let case = Case::new("short-sha");
    write_agy(&case);
    write_droid(&case);
    let sha = "a".repeat(127);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &sha)));
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: antigravity-cli: BUMP sha512 {sha} is not a 128-character lowercase hex digest"),
    );
}

#[test]
fn refuses_a_url_with_a_disallowed_character() {
    let case = Case::new("bad-url-char");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", "https://storage.example/cli?x=1", &new_sha())));
    assert_refused(&case, "bump-stale-pins: REFUSED: antigravity-cli: BUMP url https://storage.example/cli?x=1 is not a plain https url");
}

#[test]
fn exits_2_when_the_pin_file_cannot_be_replaced() {
    let case = Case::new("mv-fails");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    let shim = shim(&case, "mv", "#!/bin/sh\nexit 1\n");
    let (agy_before, _) = snapshot(&case);

    let out = case.run_with_path(false, Some(&shim));
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "planner exited {}; stderr: {err}", out.status);
    let expected = format!("bump-stale-pins: IO: cannot replace {}; earlier pins may already be written", agy_dockerfile(&case));
    assert!(
        err.lines().any(|line| line == expected),
        "stderr misses the IO line; saw: {}",
        err.lines().filter(|line| line.contains("REFUSED") || line.contains("IO")).collect::<Vec<_>>().join(" | ")
    );
    assert!(case.read("docker/antigravity-cli/Dockerfile") == agy_before, "the failed replace changed the Dockerfile");
}

#[test]
fn exits_2_when_the_temp_sibling_cannot_be_created() {
    let case = Case::new("mktemp-fails");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    let shim = shim(&case, "mktemp", "#!/bin/sh\nexit 1\n");
    let (agy_before, _) = snapshot(&case);

    let out = case.run_with_path(false, Some(&shim));
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "planner exited {}", out.status);
    let expected = format!("bump-stale-pins: cannot create a temp sibling for {}", agy_dockerfile(&case));
    assert!(
        err.lines().any(|line| line == expected),
        "stderr misses the mktemp line; saw: {}",
        err.lines()
            .filter(|line| line.contains("REFUSED") || line.contains("IO") || line.contains("temp sibling"))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    assert!(case.read("docker/antigravity-cli/Dockerfile") == agy_before, "the failed write changed the Dockerfile");
}

#[test]
fn exits_2_when_the_temp_sibling_cannot_be_written() {
    let case = Case::new("temp-write-fails");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    // mktemp returns a directory path, so the write redirect fails with EISDIR.
    let shim = shim(&case, "mktemp", "#!/bin/sh\nmkdir -p \"$2\"\nprintf '%s\\n' \"$2\"\n");
    let (agy_before, _) = snapshot(&case);

    let out = case.run_with_path(false, Some(&shim));
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "planner exited {}", out.status);
    let expected =
        format!("bump-stale-pins: IO: cannot write a temp sibling for {}; earlier pins may already be written", agy_dockerfile(&case));
    assert!(
        err.lines().any(|line| line == expected),
        "stderr misses the temp-write line; saw: {}",
        err.lines()
            .filter(|line| line.contains("REFUSED") || line.contains("IO") || line.contains("temp sibling"))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    assert!(case.read("docker/antigravity-cli/Dockerfile") == agy_before, "the failed write changed the Dockerfile");
}

#[test]
fn exits_2_when_the_temp_sibling_mode_cannot_be_matched() {
    let case = Case::new("chmod-fails");
    write_agy(&case);
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    let shim = shim(&case, "chmod", "#!/bin/sh\nexit 1\n");
    let (agy_before, _) = snapshot(&case);

    let out = case.run_with_path(false, Some(&shim));
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "planner exited {}", out.status);
    let expected = format!(
        "bump-stale-pins: IO: cannot match the mode of {} on its temp sibling; earlier pins may already be written",
        agy_dockerfile(&case)
    );
    assert!(
        err.lines().any(|line| line == expected),
        "stderr misses the chmod line; saw: {}",
        err.lines().filter(|line| line.contains("REFUSED") || line.contains("IO") || line.contains("mode")).collect::<Vec<_>>().join(" | ")
    );
    assert!(case.read("docker/antigravity-cli/Dockerfile") == agy_before, "the failed chmod changed the Dockerfile");
}

/// True when `line` holds `ver` as a whole version token: no digit or `<digit>.` before
/// it, and no digit or `.<digit>` after it, so `GitHub Copilot CLI 1.0.105.` holds
/// `1.0.105` while `21.3.0`, `1.3.01`, `0.1.3.0` and `1.3.0.5` do not hold `1.3.0`.
fn holds_version(line: &str, ver: &str) -> bool {
    let bytes = line.as_bytes();
    // Every start, overlapping ones included: `match_indices` skips those, and the planner's
    // own scan tries them.
    (0..bytes.len()).filter(|&i| bytes[i..].starts_with(ver.as_bytes())).any(|i| {
        let digit_at = |j: Option<usize>| j.is_some_and(|j| bytes[j].is_ascii_digit());
        let dot_before = i.checked_sub(1).is_some_and(|j| bytes[j] == b'.');
        let left_ok = !digit_at(i.checked_sub(1)) && !(dot_before && digit_at(i.checked_sub(2)));
        let right_ok = match bytes.get(i + ver.len()).copied() {
            Some(b) if b.is_ascii_digit() => false,
            Some(b'.') => !matches!(bytes.get(i + ver.len() + 1), Some(b) if b.is_ascii_digit()),
            _ => true,
        };
        left_ok && right_ok
    })
}

fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('#') || trimmed.starts_with("//")
}

#[test]
fn refuses_a_bump_that_leaves_the_old_pin_on_a_line_it_does_not_move() {
    let case = Case::new("second-pin-line");
    write_agy(&case);
    write_droid(&case);
    fs::create_dir_all(case.root.join("docker/copilot-cli")).unwrap();
    let copilot = "FROM scratch\nRUN npm install -g @github/copilot@1.0.105\nRUN <<'SH'\n[ \"$got\" = \"GitHub Copilot CLI 1.0.105.\" ] || exit 1\nSH\n";
    case.write("docker/copilot-cli/Dockerfile", copilot);
    // copilot-cli is planned last, so the agy and droid pins have already planned cleanly
    // when the refusal fires: nothing may be written for them either.
    write_log(
        &case,
        &format!(
            "{HEADER}pin-staleness: STALE: antigravity-cli (1.5.0 != 1.5.2), droid (1.3.0 != 1.5.0), copilot-cli (1.0.105 != 1.0.106)\n{}",
            bump_line("1.5.2", AGY_NEW_URL, &new_sha())
        ),
    );
    let file = case.docker().join("copilot-cli/Dockerfile").display().to_string();
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: copilot-cli: pin 1.0.105 also appears on line 4 of {file}, a line this planner does not move"),
    );
    assert!(case.read("docker/copilot-cli/Dockerfile") == copilot, "a refused plan rewrote the copilot Dockerfile");
}

#[test]
fn a_comment_or_a_longer_version_holding_the_old_pin_does_not_refuse() {
    let case = Case::new("pin-in-comment");
    write_agy(&case);
    case.write(
        "docker/droid/Dockerfile",
        "FROM scratch\n# measured on droid 1.3.0\nRUN bash <<'SH'\n  // pinned at 1.3.0 earlier\n\t# measured on droid 1.3.0\necho 21.3.0 1.3.01 1.3.0.5 0.1.3.0\nversion=1.3.0\nSH\n",
    );
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));

    let out = case.run(false);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "planner refused a comment or a longer version; saw: {err}");
    let droid = case.read("docker/droid/Dockerfile");
    assert_eq!(line_starting(&droid, "version="), "version=1.5.0", "droid version line");
    assert!(droid.contains("# measured on droid 1.3.0\n"), "the comment line changed");
    assert!(droid.contains("echo 21.3.0 1.3.01 1.3.0.5 0.1.3.0\n"), "the longer-version line changed");
}

#[test]
fn refuses_an_old_pin_repeated_on_an_npm_pin_line() {
    let case = Case::new("npm-pin-twice");
    write_agy(&case);
    write_droid(&case);
    fs::create_dir_all(case.root.join("docker/copilot-cli")).unwrap();
    let copilot =
        "FROM scratch\nRUN npm install -g @github/copilot@1.0.105 && npm install -g @example/helper@1.0.105  # helper tracks 1.0.105\n";
    case.write("docker/copilot-cli/Dockerfile", copilot);
    write_log(
        &case,
        &format!(
            "{HEADER}pin-staleness: STALE: antigravity-cli (1.5.0 != 1.5.2), droid (1.3.0 != 1.5.0), copilot-cli (1.0.105 != 1.0.106)\n{}",
            bump_line("1.5.2", AGY_NEW_URL, &new_sha())
        ),
    );
    let file = case.docker().join("copilot-cli/Dockerfile").display().to_string();
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: copilot-cli: pin 1.0.105 appears again on line 2 of {file}, beside the pin this planner moves"),
    );
    assert!(case.read("docker/copilot-cli/Dockerfile") == copilot, "a refused plan rewrote the copilot Dockerfile");
}

#[test]
fn refuses_an_old_pin_repeated_on_a_version_pin_line() {
    let case = Case::new("version-pin-twice");
    write_agy(&case);
    case.write("docker/droid/Dockerfile", "FROM scratch\nRUN bash <<'SH'\nset -euo pipefail\nversion=1.3.0 && echo 1.3.0\nSH\n");
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    let file = case.docker().join("droid/Dockerfile").display().to_string();
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: droid: pin 1.3.0 appears again on line 4 of {file}, beside the pin this planner moves"),
    );
}

#[test]
fn refuses_an_old_pin_repeated_on_the_url_line() {
    let case = Case::new("url-pin-twice");
    case.write(
        "docker/antigravity-cli/Dockerfile",
        &format!(
            "FROM scratch\nRUN bash <<'SH'\nset -euo pipefail\nversion=1.5.0\nurl={AGY_OLD_URL} && echo 1.5.0\nsha512={}\nSH\n",
            old_sha()
        ),
    );
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    assert_refused(
        &case,
        &format!(
            "bump-stale-pins: REFUSED: antigravity-cli: pin 1.5.0 appears again on line 5 of {}, beside the pin this planner moves",
            agy_dockerfile(&case)
        ),
    );
}

#[test]
fn refuses_an_old_pin_repeated_on_the_sha512_line() {
    let case = Case::new("sha-pin-twice");
    case.write(
        "docker/antigravity-cli/Dockerfile",
        &format!(
            "FROM scratch\nRUN bash <<'SH'\nset -euo pipefail\nversion=1.5.0\nurl={AGY_OLD_URL}\nsha512={} && echo 1.5.0\nSH\n",
            old_sha()
        ),
    );
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    assert_refused(
        &case,
        &format!(
            "bump-stale-pins: REFUSED: antigravity-cli: pin 1.5.0 appears again on line 6 of {}, beside the pin this planner moves",
            agy_dockerfile(&case)
        ),
    );
}

#[test]
fn refuses_a_live_install_hidden_after_a_quoted_hash_on_the_pin_line() {
    let case = Case::new("quoted-hash");
    write_agy(&case);
    write_droid(&case);
    fs::create_dir_all(case.root.join("docker/copilot-cli")).unwrap();
    case.write(
        "docker/copilot-cli/Dockerfile",
        "FROM scratch\nRUN npm install -g @github/copilot@1.0.105 && echo \"a # b\" && npm install -g @github/copilot@1.0.105\n",
    );
    write_log(
        &case,
        &format!(
            "{HEADER}pin-staleness: STALE: antigravity-cli (1.5.0 != 1.5.2), droid (1.3.0 != 1.5.0), copilot-cli (1.0.105 != 1.0.106)\n{}",
            bump_line("1.5.2", AGY_NEW_URL, &new_sha())
        ),
    );
    let file = case.docker().join("copilot-cli/Dockerfile").display().to_string();
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: copilot-cli: pin 1.0.105 appears again on line 2 of {file}, beside the pin this planner moves"),
    );
}

#[test]
fn refuses_an_inline_comment_naming_the_old_pin_on_a_moved_line() {
    let case = Case::new("tail-names-pin");
    case.write(
        "docker/antigravity-cli/Dockerfile",
        &format!(
            "FROM scratch\nRUN bash <<'SH'\nset -euo pipefail\nversion=1.5.0  # agy 1.5.0\nurl={AGY_OLD_URL}\nsha512={}\nSH\n",
            old_sha()
        ),
    );
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    assert_refused(
        &case,
        &format!(
            "bump-stale-pins: REFUSED: antigravity-cli: pin 1.5.0 appears again on line 4 of {}, beside the pin this planner moves",
            agy_dockerfile(&case)
        ),
    );
}

#[test]
fn refuses_an_inline_comment_naming_the_old_pin_on_the_url_line() {
    let case = Case::new("url-tail-names-pin");
    case.write(
        "docker/antigravity-cli/Dockerfile",
        &format!(
            "FROM scratch\nRUN bash <<'SH'\nset -euo pipefail\nversion=1.5.0\nurl={AGY_OLD_URL}  # was 1.5.0\nsha512={}\nSH\n",
            old_sha()
        ),
    );
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    assert_refused(
        &case,
        &format!(
            "bump-stale-pins: REFUSED: antigravity-cli: pin 1.5.0 appears again on line 5 of {}, beside the pin this planner moves",
            agy_dockerfile(&case)
        ),
    );
}

#[test]
fn refuses_an_inline_comment_naming_the_old_pin_on_the_sha512_line() {
    let case = Case::new("sha-tail-names-pin");
    case.write(
        "docker/antigravity-cli/Dockerfile",
        &format!(
            "FROM scratch\nRUN bash <<'SH'\nset -euo pipefail\nversion=1.5.0\nurl={AGY_OLD_URL}\nsha512={}  # digest of 1.5.0\nSH\n",
            old_sha()
        ),
    );
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    assert_refused(
        &case,
        &format!(
            "bump-stale-pins: REFUSED: antigravity-cli: pin 1.5.0 appears again on line 6 of {}, beside the pin this planner moves",
            agy_dockerfile(&case)
        ),
    );
}

#[test]
fn refuses_an_inline_comment_naming_the_old_pin_on_an_npm_pin_line() {
    let case = Case::new("npm-tail-names-pin");
    write_agy(&case);
    write_droid(&case);
    fs::create_dir_all(case.root.join("docker/copilot-cli")).unwrap();
    case.write("docker/copilot-cli/Dockerfile", "FROM scratch\nRUN npm install -g @github/copilot@1.0.105  # tracks 1.0.105\n");
    write_log(
        &case,
        &format!(
            "{HEADER}pin-staleness: STALE: antigravity-cli (1.5.0 != 1.5.2), droid (1.3.0 != 1.5.0), copilot-cli (1.0.105 != 1.0.106)\n{}",
            bump_line("1.5.2", AGY_NEW_URL, &new_sha())
        ),
    );
    let file = case.docker().join("copilot-cli/Dockerfile").display().to_string();
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: copilot-cli: pin 1.0.105 appears again on line 2 of {file}, beside the pin this planner moves"),
    );
}

#[test]
fn refuses_a_live_pin_that_follows_a_longer_version_on_the_same_line() {
    let case = Case::new("pin-after-longer");
    write_agy(&case);
    case.write("docker/droid/Dockerfile", "FROM scratch\nRUN bash <<'SH'\necho 21.3.0 then 1.3.0\nversion=1.3.0\nSH\n");
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    let file = case.docker().join("droid/Dockerfile").display().to_string();
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: droid: pin 1.3.0 also appears on line 3 of {file}, a line this planner does not move"),
    );
}

/// Every pinned leg under `docker`, as `(leg, pin)`: the version on its one
/// `npm install -g <pkg>@<digit…>` line or its `version=` line. A leg with neither is
/// unpinned (`@latest`, a moving tag, a bare install) and is not the planner's.
fn tree_pins(docker: &Path) -> Vec<(String, String)> {
    let mut pins = Vec::new();
    for entry in fs::read_dir(docker).unwrap() {
        let dir = entry.unwrap().path();
        let dockerfile = dir.join("Dockerfile");
        if !dockerfile.is_file() {
            continue;
        }
        let leg = dir.file_name().unwrap().to_string_lossy().into_owned();
        let body = fs::read_to_string(&dockerfile).unwrap();
        let mut found = Vec::new();
        for line in body.lines().filter(|line| !is_comment(line)) {
            if let Some(rest) = line.split("npm install -g ").nth(1) {
                let token = rest.split_whitespace().next().unwrap_or("");
                if let Some((_, ver)) = token.rsplit_once('@')
                    && ver.starts_with(|c: char| c.is_ascii_digit())
                {
                    found.push(ver.to_string());
                }
            }
            if let Some(ver) = line.strip_prefix("version=") {
                found.push(ver.split_whitespace().next().unwrap_or("").to_string());
            }
        }
        assert!(found.len() <= 1, "{leg}: more than one pin line: {found:?}");
        if let Some(pin) = found.pop() {
            pins.push((leg, pin));
        }
    }
    pins.sort();
    pins
}

/// The pin with its last core component raised by one: a version the planner accepts as
/// an upgrade.
fn next_version(pin: &str) -> String {
    let core = pin.split(['-', '+']).next().unwrap();
    let (head, last) = core.rsplit_once('.').unwrap();
    format!("{head}.{}", last.parse::<u64>().unwrap() + 1)
}

/// Positive and negative controls for the real-tree sweep's own scanners, so a scanner
/// that finds nothing cannot keep that sweep green.
#[test]
fn the_sweep_scanners_flag_a_live_pin_and_pass_the_rest() {
    assert!(holds_version("[ \"$got\" = \"GitHub Copilot CLI 1.0.105.\" ] || exit 1", "1.0.105"));
    assert!(holds_version("echo pins v1.3.0", "1.3.0"));
    assert!(!holds_version("echo 21.3.0 1.3.01 1.3.0.5 0.1.3.0", "1.3.0"));
    assert!(holds_version("curl -O https://x.example/copilot.1.3.0.tgz", "1.3.0"));
    assert!(holds_version("echo 21.3.0 then 1.3.0", "1.3.0"));
    assert!(holds_version("1.0-b1.0-b1.0-b", "1.0-b1.0-b"), "the second start overlaps the first");
    assert!(is_comment("  # measured on 1.3.0"));
    assert!(is_comment("\t// measured on 1.3.0"));
    assert!(!is_comment("version=1.3.0  # pinned"));

    let case = Case::new("sweep-controls");
    fs::create_dir_all(case.docker().join("kimi")).unwrap();
    fs::create_dir_all(case.docker().join("codex")).unwrap();
    case.write("docker/antigravity-cli/Dockerfile", "FROM scratch\n# version=0.1.0\nversion=1.5.0\n");
    case.write("docker/droid/Dockerfile", "FROM scratch\nRUN npm install -g cline || echo note\n");
    // This finder skips a commented-out install; the planner and the check count one on
    // the same package as a second pin line and refuse the file.
    case.write(
        "docker/kimi/Dockerfile",
        "FROM scratch\n# RUN npm install -g @moonshot-ai/kimi-code@2.4.0\nRUN npm install -g @moonshot-ai/kimi-code@2.5.0\n",
    );
    case.write("docker/codex/Dockerfile", "FROM scratch\nRUN npm install -g @openai/codex@latest\n");
    assert_eq!(
        tree_pins(&case.docker()),
        vec![("antigravity-cli".to_string(), "1.5.0".to_string()), ("kimi".to_string(), "2.5.0".to_string())]
    );
    assert_eq!(next_version("1.0.105"), "1.0.106");
    assert_eq!(next_version("0.99.0"), "0.99.1");
}

#[test]
fn every_real_pinned_leg_moves_its_whole_pin() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/docker");
    let pins = tree_pins(&src);
    let legs: Vec<&str> = pins.iter().map(|(leg, _)| leg.as_str()).collect();
    // Non-vacuous: the sweep must reach the npm, version and three-field shapes.
    for must in ["antigravity-cli", "copilot-cli", "droid"] {
        assert!(legs.contains(&must), "the sweep found no pin for {must}; found {legs:?}");
    }
    // The sweep covers exactly the legs the staleness check watches, so a leg the check
    // pins in a shape this finder misses cannot drop out of the sweep unnoticed.
    let check = fs::read_to_string(src.join("pin-staleness.sh")).unwrap();
    let legs_line = check.lines().find_map(|line| line.strip_prefix("LEGS=(")).expect("pin-staleness.sh has no LEGS=( line");
    let mut watched: Vec<&str> = legs_line.trim_end_matches(')').split_whitespace().collect();
    watched.sort_unstable();
    assert_eq!(legs, watched, "the sweep's pinned legs differ from pin-staleness.sh's LEGS");

    let case = Case::new("real-tree");
    let mut entries = Vec::new();
    for (leg, pin) in &pins {
        fs::create_dir_all(case.docker().join(leg)).unwrap();
        fs::copy(src.join(leg).join("Dockerfile"), case.docker().join(leg).join("Dockerfile")).unwrap();
        entries.push(format!("{leg} ({pin} != {})", next_version(pin)));
    }
    // The url is built from the version it bumps to, as the vendor's is: a constant one
    // would read as a surviving pin on the day the real pin reaches its version.
    let agy_new = next_version(&pins.iter().find(|(leg, _)| leg == "antigravity-cli").unwrap().1);
    let agy_url = format!("https://storage.example/antigravity-cli/{agy_new}-222/linux-x64/cli.tar.gz");
    write_log(&case, &format!("{HEADER}pin-staleness: STALE: {}\n{}", entries.join(", "), bump_line(&agy_new, &agy_url, &new_sha())));

    let out = case.run(false);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the planner refused the real tree's pins; saw: {err}");
    let plan = String::from_utf8_lossy(&out.stdout);
    assert_eq!(plan.lines().filter(|line| line.starts_with("bump-stale-pins: PLAN: ")).count(), pins.len(), "plan lines: {plan}");
    let bumped: Vec<(String, String)> = pins.iter().map(|(leg, pin)| (leg.clone(), next_version(pin))).collect();
    assert_eq!(tree_pins(&case.docker()), bumped, "a leg's pin line does not hold its new version");

    for (leg, pin) in &pins {
        let body = case.read(&format!("docker/{leg}/Dockerfile"));
        let survivors: Vec<(usize, &str)> = body
            .lines()
            .enumerate()
            .filter(|(_, line)| !is_comment(line) && holds_version(line, pin))
            .map(|(i, line)| (i + 1, line))
            .collect();
        assert!(survivors.is_empty(), "{leg}: the bump left pin {pin} on live lines {survivors:?}");
    }
}

#[test]
fn dry_run_refuses_both_surviving_pin_shapes() {
    let case = Case::new("dry-run-guard");
    write_agy(&case);
    case.write("docker/droid/Dockerfile", "FROM scratch\nRUN bash <<'SH'\necho 1.3.0\nversion=1.3.0\nSH\n");
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    let file = case.docker().join("droid/Dockerfile").display().to_string();
    let out = case.run(true);
    assert_eq!(out.status.code(), Some(1), "a dry run accepted an unmoved copy");
    assert!(String::from_utf8_lossy(&out.stderr).lines().any(|line| line
        == format!("bump-stale-pins: REFUSED: droid: pin 1.3.0 also appears on line 3 of {file}, a line this planner does not move")));
    case.write("docker/droid/Dockerfile", "FROM scratch\nRUN bash <<'SH'\nversion=1.3.0 && echo 1.3.0\nSH\n");
    let out = case.run(true);
    assert_eq!(out.status.code(), Some(1), "a dry run accepted a repeat on the pin line");
    assert!(String::from_utf8_lossy(&out.stderr).lines().any(|line| line
        == format!("bump-stale-pins: REFUSED: droid: pin 1.3.0 appears again on line 3 of {file}, beside the pin this planner moves")));
    assert!(out.stdout.is_empty(), "a refused dry run printed a plan line");
}

#[test]
fn the_planner_tries_an_overlapping_second_start() {
    let case = Case::new("overlap");
    write_agy(&case);
    case.write("docker/droid/Dockerfile", "FROM scratch\nRUN bash <<'SH'\necho 1.0-b1.0-b1.0-b\nversion=1.0-b1.0-b\nSH\n");
    write_log(
        &case,
        &format!(
            "{HEADER}pin-staleness: STALE: antigravity-cli (1.5.0 != 1.5.2), droid (1.0-b1.0-b != 1.0.1)\n{}",
            bump_line("1.5.2", AGY_NEW_URL, &new_sha())
        ),
    );
    let file = case.docker().join("droid/Dockerfile").display().to_string();
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: droid: pin 1.0-b1.0-b also appears on line 3 of {file}, a line this planner does not move"),
    );
}

#[test]
fn refuses_a_pin_inside_a_dotted_artifact_name_on_another_line() {
    let case = Case::new("dotted-artifact");
    write_agy(&case);
    case.write(
        "docker/droid/Dockerfile",
        "FROM scratch\nRUN curl -fsSLO https://x.example/droid.1.3.0.tgz\nRUN bash <<'SH'\nversion=1.3.0\nSH\n",
    );
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    let file = case.docker().join("droid/Dockerfile").display().to_string();
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: droid: pin 1.3.0 also appears on line 2 of {file}, a line this planner does not move"),
    );
}

#[test]
fn refuses_the_old_sha512_on_a_line_the_plan_does_not_move() {
    let case = Case::new("old-sha-elsewhere");
    case.write(
        "docker/antigravity-cli/Dockerfile",
        &format!(
            "FROM scratch\nRUN bash <<'SH'\nset -euo pipefail\nversion=1.5.0\nurl={AGY_OLD_URL}\nsha512={sha}\necho \"{sha}  /tmp/agy.tar.gz\" | sha512sum -c\nSH\n",
            sha = old_sha()
        ),
    );
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    assert_refused(
        &case,
        &format!(
            "bump-stale-pins: REFUSED: antigravity-cli: the old sha512 also appears on line 7 of {}, a line this planner does not move",
            agy_dockerfile(&case)
        ),
    );
}

#[test]
fn refuses_a_second_install_of_the_package_at_another_version_on_the_pin_line() {
    let case = Case::new("second-install-other-version");
    write_agy(&case);
    write_droid(&case);
    fs::create_dir_all(case.root.join("docker/copilot-cli")).unwrap();
    case.write(
        "docker/copilot-cli/Dockerfile",
        "FROM scratch\nRUN npm install -g @github/copilot@1.0.105 && npm install -g @github/copilot@1.0.104\n",
    );
    write_log(
        &case,
        &format!(
            "{HEADER}pin-staleness: STALE: antigravity-cli (1.5.0 != 1.5.2), droid (1.3.0 != 1.5.0), copilot-cli (1.0.105 != 1.0.106)\n{}",
            bump_line("1.5.2", AGY_NEW_URL, &new_sha())
        ),
    );
    let file = case.docker().join("copilot-cli/Dockerfile").display().to_string();
    assert_refused(
        &case,
        &format!(
            "bump-stale-pins: REFUSED: copilot-cli: package @github/copilot appears again on line 2 of {file}; this planner moves one install"
        ),
    );
}

#[test]
fn refuses_the_package_with_no_version_on_the_pin_line() {
    let case = Case::new("bare-package-pin-line");
    write_agy(&case);
    write_droid(&case);
    fs::create_dir_all(case.root.join("docker/copilot-cli")).unwrap();
    case.write(
        "docker/copilot-cli/Dockerfile",
        "FROM scratch\nRUN npm install -g @github/copilot@1.0.105 && npm update --global @github/copilot\n",
    );
    write_log(
        &case,
        &format!(
            "{HEADER}pin-staleness: STALE: antigravity-cli (1.5.0 != 1.5.2), droid (1.3.0 != 1.5.0), copilot-cli (1.0.105 != 1.0.106)\n{}",
            bump_line("1.5.2", AGY_NEW_URL, &new_sha())
        ),
    );
    let file = case.docker().join("copilot-cli/Dockerfile").display().to_string();
    assert_refused(
        &case,
        &format!(
            "bump-stale-pins: REFUSED: copilot-cli: package @github/copilot appears again on line 2 of {file}; this planner moves one install"
        ),
    );
}

#[test]
fn refuses_the_package_installed_in_another_spelling_on_another_line() {
    let case = Case::new("other-spelling-install");
    write_agy(&case);
    write_droid(&case);
    fs::create_dir_all(case.root.join("docker/copilot-cli")).unwrap();
    case.write(
        "docker/copilot-cli/Dockerfile",
        "FROM scratch\nRUN npm install -g @github/copilot@1.0.105\nRUN npm i -g @github/copilot@1.0.104\n",
    );
    write_log(
        &case,
        &format!(
            "{HEADER}pin-staleness: STALE: antigravity-cli (1.5.0 != 1.5.2), droid (1.3.0 != 1.5.0), copilot-cli (1.0.105 != 1.0.106)\n{}",
            bump_line("1.5.2", AGY_NEW_URL, &new_sha())
        ),
    );
    let file = case.docker().join("copilot-cli/Dockerfile").display().to_string();
    assert_refused(
        &case,
        &format!(
            "bump-stale-pins: REFUSED: copilot-cli: package @github/copilot also appears on line 3 of {file}; this planner moves one install"
        ),
    );
}

#[test]
fn a_path_or_comment_naming_the_package_does_not_refuse() {
    let case = Case::new("package-in-path");
    write_agy(&case);
    write_droid(&case);
    fs::create_dir_all(case.root.join("docker/copilot-cli")).unwrap();
    case.write(
        "docker/copilot-cli/Dockerfile",
        "FROM scratch\n# reads @github/copilot back\nRUN npm install -g @github/copilot@1.0.105\nRUN echo \"$root/@github/copilot/package.json\"\n",
    );
    write_log(
        &case,
        &format!(
            "{HEADER}pin-staleness: STALE: antigravity-cli (1.5.0 != 1.5.2), droid (1.3.0 != 1.5.0), copilot-cli (1.0.105 != 1.0.106)\n{}",
            bump_line("1.5.2", AGY_NEW_URL, &new_sha())
        ),
    );
    let out = case.run(false);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "planner refused a path or comment naming the package; saw: {err}");
}

#[test]
fn refuses_an_empty_sha512_line() {
    let case = Case::new("empty-sha");
    case.write(
        "docker/antigravity-cli/Dockerfile",
        &format!("FROM scratch\nRUN bash <<'SH'\nversion=1.5.0\nurl={AGY_OLD_URL}\nsha512=\nSH\n"),
    );
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    assert_refused(
        &case,
        &format!("bump-stale-pins: REFUSED: antigravity-cli: the sha512= line in {} holds no digest", agy_dockerfile(&case)),
    );
}

#[test]
fn refuses_a_missing_sha512_line_in_the_three_field_pin() {
    let case = Case::new("no-sha");
    case.write("docker/antigravity-cli/Dockerfile", &format!("FROM scratch\nversion=1.5.0\nurl={AGY_OLD_URL}\n"));
    write_droid(&case);
    write_log(&case, &format!("{HEADER}{}{}", stale_line(), bump_line("1.5.2", AGY_NEW_URL, &new_sha())));
    assert_refused(
        &case,
        &format!(
            "bump-stale-pins: REFUSED: antigravity-cli: expected exactly one '^sha512=<digest>' line in {}, found 0",
            agy_dockerfile(&case)
        ),
    );
}
