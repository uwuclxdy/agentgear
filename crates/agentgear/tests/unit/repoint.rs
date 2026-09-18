//! `repoint_install_paths` unit tests: the byte-surgical installPath re-root a
//! host runs over Claude Code's `installed_plugins.json` when a per-session
//! config dir dies and leaves the recorded paths dangling. The contract: only
//! the exact quoted values the host's remap targets change; every other byte
//! (formatting, key order, unknown fields, a foreign schema) survives untouched,
//! a no-change run never writes the file, and a concurrent write restarts the
//! pass instead of being dropped.

use std::path::Path;

use super::{Remap, RepointReport, repoint_install_paths};

/// The remap a host with per-session config dirs supplies: every path under
/// `sessions_root` re-roots at `twin_root` by the `plugins/` suffix, keeps a
/// still-resolving path, and names a targeted path whose twin is missing.
fn remap_for<'a>(sessions_root: &'a str, twin_root: &'a Path) -> impl FnMut(&str) -> Remap + 'a {
    move |p: &str| {
        if !p.starts_with(sessions_root) {
            return Remap::Keep;
        }
        if std::path::Path::new(p).exists() {
            return Remap::Keep;
        }
        let Some((_, suffix)) = p.split_once("plugins/") else {
            return Remap::Skip("no plugins/ segment".to_string());
        };
        let twin = twin_root.join(suffix);
        if twin.exists() { Remap::Rewrite(twin.to_string_lossy().into_owned()) } else { Remap::Skip("no twin".to_string()) }
    }
}

const SESSIONS: &str = "/home/u/.sessions";

#[test]
fn repoint_rewrites_a_dead_path_to_its_twin_and_names_a_missing_one() {
    let root = crate::scratch::path("ez-repoint-rewrite");
    std::fs::create_dir_all(&root).unwrap();

    // The shared twin of the first recorded path exists; the second's does not.
    let twin_root = root.join("claude-plugins");
    std::fs::create_dir_all(twin_root.join("cache/agenticat/agents/a6261ea74c14")).unwrap();

    let runtime = "/home/u/.sessions/D0/runtime-700698-0/plugins/cache/agenticat/agents/a6261ea74c14";
    let missing = "/home/u/.sessions/D0/runtime-672416-5/plugins/cache/claude-plugins-official/security-guidance/2.0.8";
    let original = format!(
        r#"{{
  "version": 2,
  "plugins": {{
    "agents@agenticat": [
      {{ "scope": "user", "installPath": "{runtime}" }},
      {{ "scope": "project", "projectPath": "/home/u", "installPath": "{runtime}" }}
    ],
    "security-guidance@claude-plugins-official": [
      {{ "scope": "user", "installPath": "{missing}" }}
    ]
  }}
}}
"#
    );
    let registry = root.join("installed_plugins.json");
    std::fs::write(&registry, &original).unwrap();

    let report = repoint_install_paths(&registry, remap_for(SESSIONS, &twin_root)).unwrap();

    let twin = twin_root.join("cache/agenticat/agents/a6261ea74c14");
    assert_eq!(
        report.rewritten,
        vec![super::Repointed { from: runtime.to_string(), to: twin.to_string_lossy().into_owned() }],
        "one dead path with a twin is rewritten once, however many entries record it"
    );
    assert_eq!(
        report.skipped,
        vec![super::RepointSkip { path: missing.to_string(), reason: "no twin".to_string() }],
        "a targeted path whose twin is missing is named, not rewritten"
    );

    let expected = original.replace(&format!("\"{runtime}\""), &format!("\"{}\"", twin.to_string_lossy().into_owned()));
    assert_eq!(std::fs::read_to_string(&registry).unwrap(), expected, "only the quoted path values change; every other byte survives");
}

#[test]
fn repoint_keeps_file_order_across_several_rows_and_dedups_duplicate_skips() {
    let root = crate::scratch::path("ez-repoint-order");
    std::fs::create_dir_all(&root).unwrap();
    let twin_root = root.join("claude-plugins");
    std::fs::create_dir_all(twin_root.join("cache/a/1")).unwrap();
    std::fs::create_dir_all(twin_root.join("cache/c/3")).unwrap();

    let a = "/home/u/.sessions/D0/runtime-1-0/plugins/cache/a/1";
    let b = "/home/u/.sessions/D0/runtime-2-0/plugins/cache/b/2";
    let c = "/home/u/.sessions/D0/runtime-3-0/plugins/cache/c/3";
    let original = format!(
        r#"{{"version":2,"plugins":{{"x@x":[{{"installPath":"{a}"}},{{"installPath":"{b}"}},{{"installPath":"{b}"}},{{"installPath":"{c}"}}]}}}}"#
    );
    let registry = root.join("installed_plugins.json");
    std::fs::write(&registry, &original).unwrap();

    let report = repoint_install_paths(&registry, remap_for(SESSIONS, &twin_root)).unwrap();

    let twin_of = |p: &str| twin_root.join(p.split_once("plugins/").unwrap().1).to_string_lossy().into_owned();
    assert_eq!(
        report.rewritten.iter().map(|r| r.from.as_str()).collect::<Vec<_>>(),
        vec![a, c],
        "rewrites follow the file's value order; b's twin is missing so it never joins the list"
    );
    assert_eq!(
        report.rewritten.iter().map(|r| r.to.as_str()).collect::<Vec<_>>(),
        vec![twin_of(a), twin_of(c)],
        "each rewrite lands at its own twin"
    );
    assert_eq!(
        report.skipped,
        vec![super::RepointSkip { path: b.to_string(), reason: "no twin".to_string() }],
        "a duplicate missing spelling is one skip row"
    );
    let expected =
        original.replace(&format!("\"{a}\""), &format!("\"{}\"", twin_of(a))).replace(&format!("\"{c}\""), &format!("\"{}\"", twin_of(c)));
    assert_eq!(std::fs::read_to_string(&registry).unwrap(), expected, "b stays byte-identical where it was");
}

#[test]
fn repoint_leaves_a_clean_registry_untouched_and_unwritten() {
    let root = crate::scratch::path("ez-repoint-clean");
    std::fs::create_dir_all(&root).unwrap();
    let registry = root.join("installed_plugins.json");

    // Absent: a box that never installed a plugin has no registry to converge.
    let report = repoint_install_paths(&registry, remap_for(SESSIONS, &root)).unwrap();
    assert!(report.rewritten.is_empty() && report.skipped.is_empty(), "a missing registry converges nothing: {report:?}");
    assert!(!registry.exists(), "a missing registry is not created");

    let bytes = r#"{
  "version": 2,
  "plugins": {
    "claudix@claudix": [
      { "scope": "user", "installPath": "/home/u/.claude/plugins/cache/claudix/claudix/0.5.1" }
    ]
  }
}
"#;
    std::fs::write(&registry, bytes).unwrap();
    let before = std::fs::metadata(&registry).unwrap().modified().unwrap();

    let report = repoint_install_paths(&registry, remap_for(SESSIONS, &root)).unwrap();

    assert!(report.rewritten.is_empty() && report.skipped.is_empty(), "nothing targeted, nothing reported: {report:?}");
    assert_eq!(std::fs::read_to_string(&registry).unwrap(), bytes, "a no-change run never writes the file");
    assert_eq!(std::fs::metadata(&registry).unwrap().modified().unwrap(), before, "a no-change run never writes the file (mtime)");
}

#[test]
fn repoint_skips_only_never_writes() {
    let root = crate::scratch::path("ez-repoint-skips");
    std::fs::create_dir_all(&root).unwrap();
    let registry = root.join("installed_plugins.json");
    let missing = "/home/u/.sessions/D0/runtime-9-0/plugins/cache/m/1";
    let original = format!(r#"{{"version":2,"plugins":{{"m@m":[{{"installPath":"{missing}"}}]}}}}"#);
    std::fs::write(&registry, &original).unwrap();
    let before = std::fs::metadata(&registry).unwrap().modified().unwrap();

    let report = repoint_install_paths(&registry, remap_for(SESSIONS, &root)).unwrap();

    assert!(report.rewritten.is_empty(), "nothing converged: {report:?}");
    assert_eq!(report.skipped.len(), 1, "the missing twin is named");
    assert_eq!(std::fs::read_to_string(&registry).unwrap(), original, "skips never move bytes");
    assert_eq!(std::fs::metadata(&registry).unwrap().modified().unwrap(), before, "skips never write");
}

#[test]
fn repoint_converges_paths_under_a_foreign_schema() {
    let root = crate::scratch::path("ez-repoint-schema");
    std::fs::create_dir_all(&root).unwrap();
    let twin_root = root.join("claude-plugins");
    std::fs::create_dir_all(twin_root.join("cache/agenticat/agents/a6261ea74c14")).unwrap();

    let runtime = "/home/u/.sessions/D0/runtime-700698-0/plugins/cache/agenticat/agents/a6261ea74c14";
    let original = format!(r#"{{"v3":true,"installs":{{"agents@agenticat":[{{"path":"{runtime}"}}]}}}}"#);
    let registry = root.join("installed_plugins.json");
    std::fs::write(&registry, &original).unwrap();

    let report = repoint_install_paths(&registry, remap_for(SESSIONS, &twin_root)).unwrap();

    let twin = twin_root.join("cache/agenticat/agents/a6261ea74c14");
    assert_eq!(report.rewritten.len(), 1, "a schema bump renames keys, never the recorded path values: {report:?}");
    let expected = original.replace(runtime, &twin.to_string_lossy());
    assert_eq!(std::fs::read_to_string(&registry).unwrap(), expected);
}

#[test]
fn repoint_scanner_does_not_desync_on_escaped_quotes() {
    let root = crate::scratch::path("ez-repoint-escape");
    std::fs::create_dir_all(&root).unwrap();
    let twin_root = root.join("claude-plugins");
    std::fs::create_dir_all(twin_root.join("cache/agenticat/agents/a6261ea74c14")).unwrap();

    let runtime = "/home/u/.sessions/D0/runtime-700698-0/plugins/cache/agenticat/agents/a6261ea74c14";
    let original = format!(r#"{{"version":2,"plugins":{{"agents@agenticat":[{{"note":"he said \"hi\"","installPath":"{runtime}"}}]}}}}"#);
    let registry = root.join("installed_plugins.json");
    std::fs::write(&registry, &original).unwrap();

    // The scanner contract: a value reaches the remap whole, escapes included.
    // A scanner that reads the escaped quote as the terminator splits this
    // value into `he said \` + garbage, and the remap never sees it whole.
    let mut saw_whole = false;
    let mut inner = remap_for(SESSIONS, &twin_root);
    let report = repoint_install_paths(&registry, |p: &str| {
        if p == "he said \\\"hi\\\"" {
            saw_whole = true;
        }
        inner(p)
    })
    .unwrap();

    let twin = twin_root.join("cache/agenticat/agents/a6261ea74c14");
    assert!(saw_whole, "the remap must receive the escaped value whole, not split at the escaped quote");
    assert_eq!(
        report.rewritten,
        vec![super::Repointed { from: runtime.to_string(), to: twin.to_string_lossy().into_owned() }],
        "an escaped quote earlier in the file must not shift the value scan: {report:?}"
    );
    let expected = original.replace(runtime, &twin.to_string_lossy());
    assert_eq!(std::fs::read_to_string(&registry).unwrap(), expected, "the escaped quote survives byte-identical");
}

#[test]
fn repoint_scans_keys_and_values_alike_and_preserves_non_ascii_bytes() {
    let root = crate::scratch::path("ez-repoint-keys");
    std::fs::create_dir_all(&root).unwrap();
    let twin_root = root.join("claude-plugins");
    std::fs::create_dir_all(twin_root.join("cache/a/1")).unwrap();

    // The target spelling sits in a KEY position here: a scanner that learned
    // a key/value distinction would leave it. The non-ASCII note must survive
    // byte-identical.
    let key = "/home/u/.sessions/D0/runtime-1-0/plugins/cache/a/1";
    let original = format!(r#"{{"version":2,"note":"héllo wörld","{key}":true}}"#);
    let registry = root.join("installed_plugins.json");
    std::fs::write(&registry, &original).unwrap();

    let report = repoint_install_paths(&registry, remap_for(SESSIONS, &twin_root)).unwrap();

    let twin = twin_root.join("cache/a/1");
    assert_eq!(
        report.rewritten,
        vec![super::Repointed { from: key.to_string(), to: twin.to_string_lossy().into_owned() }],
        "keys and values scan alike: {report:?}"
    );
    let expected = original.replace(key, &twin.to_string_lossy());
    assert_eq!(std::fs::read_to_string(&registry).unwrap(), expected, "the non-ASCII bytes survive untouched");
}

#[test]
fn repoint_drops_an_unterminated_final_string() {
    let root = crate::scratch::path("ez-repoint-unterminated");
    std::fs::create_dir_all(&root).unwrap();
    let registry = root.join("installed_plugins.json");
    let bytes = br#"{"version":2,"tail":"unterminated"#.as_slice();
    std::fs::write(&registry, bytes).unwrap();

    let report = repoint_install_paths(&registry, remap_for(SESSIONS, &root)).unwrap();

    assert!(report.rewritten.is_empty() && report.skipped.is_empty(), "the dangling value is dropped, not handed to the remap: {report:?}");
    assert_eq!(std::fs::read(&registry).unwrap(), bytes, "nothing changed, nothing written");
}

#[test]
fn repoint_errors_on_non_utf8_and_unreadable_registries() {
    let root = crate::scratch::path("ez-repoint-errors");
    std::fs::create_dir_all(&root).unwrap();

    // Non-UTF-8: refused with the crate error, file untouched.
    let registry = root.join("installed_plugins.json");
    std::fs::write(&registry, [0xff, 0xfe, b'"']).unwrap();
    let err = repoint_install_paths(&registry, remap_for(SESSIONS, &root)).unwrap_err();
    assert!(matches!(err, crate::Error::Io { .. }), "non-UTF-8 is an Io error, not a silent skip: {err}");
    assert_eq!(std::fs::read(&registry).unwrap(), [0xff, 0xfe, b'"'], "the bad bytes stay");

    // Unreadable (permission denied) is an error, never a silent skip.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o000)).unwrap();
        let err = repoint_install_paths(&registry, remap_for(SESSIONS, &root)).unwrap_err();
        assert!(matches!(err, crate::Error::Io { .. }), "an unreadable registry is an Io error: {err}");
        std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
}

#[test]
fn repoint_restarts_on_a_registry_changed_under_the_pass_and_refuses_a_deleted_one() {
    let root = crate::scratch::path("ez-repoint-drift");
    std::fs::create_dir_all(&root).unwrap();
    let twin_root = root.join("claude-plugins");
    std::fs::create_dir_all(twin_root.join("cache/a/1")).unwrap();
    std::fs::create_dir_all(twin_root.join("cache/b/2")).unwrap();

    let a = "/home/u/.sessions/D0/runtime-1-0/plugins/cache/a/1";
    let b = "/home/u/.sessions/D0/runtime-2-0/plugins/cache/b/2";
    let registry = root.join("installed_plugins.json");
    std::fs::write(&registry, format!(r#"{{"version":2,"plugins":{{"x@x":[{{"installPath":"{a}"}}]}}}}"#)).unwrap();

    // A concurrent writer lands `b` while the first pass is still scanning:
    // the remap is the only seam between the read and the re-read, so it is
    // the writer. The pass must restart and converge the fresh bytes, not
    // rename the stale image over the new entry.
    let mut calls = 0;
    let mut inner = remap_for(SESSIONS, &twin_root);
    let report = repoint_install_paths(&registry, |p: &str| {
        calls += 1;
        if calls == 1 {
            std::fs::write(&registry, format!(r#"{{"version":2,"plugins":{{"x@x":[{{"installPath":"{a}"}},{{"installPath":"{b}"}}]}}}}"#))
                .unwrap();
        }
        inner(p)
    })
    .unwrap();

    let a_twin = twin_root.join("cache/a/1").to_string_lossy().into_owned();
    let b_twin = twin_root.join("cache/b/2").to_string_lossy().into_owned();
    assert_eq!(
        report.rewritten.iter().map(|r| r.from.as_str()).collect::<Vec<_>>(),
        vec![a, b],
        "the restarted pass converges the fresh bytes, both paths: {report:?}"
    );
    assert!(calls > 4, "the remap ran for both passes, so the restart is observable through the call count: {calls}");
    let final_bytes = std::fs::read_to_string(&registry).unwrap();
    assert!(final_bytes.contains(&format!("\"{b_twin}\"")), "the concurrent entry survives the rename: {final_bytes}");
    assert_eq!(final_bytes.matches(&format!("\"{a_twin}\"")).count(), 1);

    // A registry deleted mid-pass is refused, never resurrected from the
    // stale image.
    std::fs::write(&registry, format!(r#"{{"version":2,"plugins":{{"x@x":[{{"installPath":"{a}"}}]}}}}"#)).unwrap();
    let mut calls = 0;
    let mut inner = remap_for(SESSIONS, &twin_root);
    let err = repoint_install_paths(&registry, |p: &str| {
        calls += 1;
        if calls == 1 {
            std::fs::remove_file(&registry).unwrap();
        }
        inner(p)
    })
    .unwrap_err();
    assert!(matches!(err, crate::Error::Io { .. }), "a deleted registry is refused, not resurrected: {err}");
    assert!(!registry.exists(), "the file stays deleted");
}

#[test]
fn repoint_self_rewrite_is_a_noop() {
    let root = crate::scratch::path("ez-repoint-self");
    std::fs::create_dir_all(&root).unwrap();
    let registry = root.join("installed_plugins.json");
    let bytes = r#"{"version":2,"plugins":{"x@x":[{"installPath":"/a"}]}}"#;
    std::fs::write(&registry, bytes).unwrap();

    let report = repoint_install_paths(&registry, |p: &str| Remap::Rewrite(p.to_string())).unwrap();

    assert!(report.rewritten.is_empty(), "rewriting a value to itself converges nothing: {report:?}");
    assert_eq!(std::fs::read_to_string(&registry).unwrap(), bytes);
}

/// The report is a plain struct a host prints from; keep it readable.
#[test]
fn report_debug_names_what_moved() {
    let report = RepointReport {
        rewritten: vec![super::Repointed { from: "/a".into(), to: "/b".into() }],
        skipped: vec![super::RepointSkip { path: "/c".into(), reason: "no twin".into() }],
    };
    let line = format!("{report:?}");
    assert!(line.contains("/a") && line.contains("/b") && line.contains("/c") && line.contains("no twin"), "{line}");
}
