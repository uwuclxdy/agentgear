//! host unit tests: `Scope`'s pure key derivation. Unix-only (needs a real
//! symlink); matches this crate's existing precedent for platform-gating
//! filesystem-symlink assertions (e.g. `cline`'s executable-bit check).

use super::*;

/// The project-scope stamp key is per-project-PATH, so a project reached via a
/// symlink and via its realpath must key the SAME marker — an unresolved raw
/// path would double-install. Built from an explicit symlink (not an assumption
/// that `TMPDIR` itself is symlinked), per the repo's own realpath-comparison
/// learning.
#[cfg(unix)]
#[test]
fn project_scope_key_is_stable_through_a_symlink() {
    let root = std::env::temp_dir().join(format!("agentgear-scope-key-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let real = root.join("real-project");
    std::fs::create_dir_all(&real).expect("create real project dir");
    let link = root.join("link-to-project");
    std::os::unix::fs::symlink(&real, &link).expect("create symlink to project dir");

    let via_real = Scope::Project { path: real.clone() }.key();
    let via_link = Scope::Project { path: link.clone() }.key();

    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(via_real, via_link, "a symlinked project path must key the same stamp marker as its realpath");
}
