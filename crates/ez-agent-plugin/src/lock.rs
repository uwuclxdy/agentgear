//! One `flock` at a well-known path, shared across every consumer of this crate.
//! Held around every mutating CLI sequence so two different tools both healing at
//! session start serialize instead of racing CC's registry (design §concurrency).

use std::fs::{self, File};
use std::path::PathBuf;

use fs4::FileExt;

use crate::error::{Error, Result};

fn lock_path() -> PathBuf {
    dirs::runtime_dir().unwrap_or_else(std::env::temp_dir).join("ez-agent-plugin.lock")
}

/// Releases the exclusive lock on drop. Closing the fd would release it too; the
/// explicit `unlock` makes the release point obvious.
pub(crate) struct LockGuard {
    file: File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

// `lock`/`unlock` are called through the trait to avoid ambiguity with the
// inherent `std::fs::File::lock` on newer toolchains.

/// Block until the shared lock is held. Blocking (not try) so a second tool waits
/// its turn rather than skipping its heal.
pub(crate) fn acquire() -> Result<LockGuard> {
    let path = lock_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = File::create(&path).map_err(|source| Error::Lock { path: path.clone(), source })?;
    FileExt::lock(&file).map_err(|source| Error::Lock { path, source })?;
    Ok(LockGuard { file })
}
