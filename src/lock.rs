//! Lock layer for `agentgc run`: one advisory exclusive lock per slot file.
//! LockFileEx on Windows, flock on Unix. The OS drops the lock when the holder dies.
use fs4::fs_std::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// A held slot. The lock is released when this value is dropped (or the process ends).
pub struct Slot {
    file: File,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// `<dir>/<pool>/<resource>.<i>.lock`
pub fn slot_path(dir: &Path, pool: &str, resource: &str, i: usize) -> PathBuf {
    dir.join(pool).join(format!("{resource}.{i}.lock"))
}

/// `<dir>/<pool>/<resource>.<i>.info`, next to the lock file.
pub fn info_path(lock: &Path) -> PathBuf {
    lock.with_extension("info")
}

/// Try to take the slot without waiting. `Ok(None)` means somebody holds it.
/// With `create`, the file and its folder are created when missing.
pub fn try_acquire(path: &Path, create: bool) -> io::Result<Option<Slot>> {
    if create {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
    }
    let file = OpenOptions::new().read(true).write(true).create(create).truncate(false).open(path)?;
    match FileExt::try_lock_exclusive(&file) {
        Ok(true) => Ok(Some(Slot { file })),
        Ok(false) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Is the slot held right now? Takes the lock and releases it at once. Never creates a file.
pub fn is_held(path: &Path) -> io::Result<bool> {
    Ok(try_acquire(path, false)?.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("agentgc-lock-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn second_acquire_fails_and_drop_releases() {
        let d = tmp("a");
        let p = slot_path(&d, "default", "r", 0);
        let s = try_acquire(&p, true).unwrap().expect("first acquire");
        assert!(try_acquire(&p, true).unwrap().is_none());
        assert!(is_held(&p).unwrap());
        drop(s);
        assert!(!is_held(&p).unwrap());
        assert!(try_acquire(&p, true).unwrap().is_some());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn other_slot_index_is_free() {
        let d = tmp("b");
        let p0 = slot_path(&d, "default", "r", 0);
        let p1 = slot_path(&d, "default", "r", 1);
        let _s0 = try_acquire(&p0, true).unwrap().unwrap();
        assert!(try_acquire(&p1, true).unwrap().is_some());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn status_check_does_not_create_files() {
        let d = tmp("c");
        assert!(is_held(&slot_path(&d, "p", "r", 0)).is_err());
        assert!(!d.exists());
    }
}
