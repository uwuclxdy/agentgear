//! Tiny shared helpers.

use std::path::{Path, PathBuf};

use crate::error::{IoContext, Result};

/// Lowercase hex of a byte slice.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Write to a temp sibling then rename onto `path`, so a reader never sees a
/// half-written file and a crash leaves the prior one intact. Both callers
/// (the config backends' `confedit`, the `claude`-gated `repoint`) are
/// feature-gated, so a build with none of them leaves this dead — same
/// shared-helper shape as the `skillsdir` module gate in `agents/mod.rs`.
#[allow(dead_code)]
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).io_ctx(|| format!("creating {}", parent.display()))?;
    }
    let tmp = tmp_sibling(path);
    std::fs::write(&tmp, bytes).io_ctx(|| format!("writing {}", tmp.display()))?;
    if let Err(source) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(crate::error::Error::Io { context: format!("renaming {} -> {}", tmp.display(), path.display()), source });
    }
    Ok(())
}

/// The pid is not decoration: `fastrand`'s only cross-process entropy is
/// `Instant::now()`, and the lifecycle flock narrows rather than excludes —
/// `lock_path()` falls back to `std::env::temp_dir()` when `XDG_RUNTIME_DIR` is
/// unset, so two processes under different `TMPDIR`s share no lock file at all and
/// can otherwise pick the same temp name for the same file.
#[allow(dead_code)]
fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".tmp.{:016x}.{}", fastrand::u64(..), std::process::id()));
    path.with_file_name(name)
}
