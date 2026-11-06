//! Red-first gate for the pin-bump planner's three-field pin support.
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
            "FROM scratch\nRUN bash <<'SH'\nset -euo pipefail\nversion=1.5.0  # pinned against agy 1.5.0\nurl={AGY_OLD_URL}  # artifact\nsha512={}  # digest\nSH\n",
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
    assert_eq!(line_starting(&agy, "version="), "version=1.5.2  # pinned against agy 1.5.0", "version line tail");
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
