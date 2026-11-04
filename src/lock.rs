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

/// `<dir>/<pool>/<resource>.gate.lock`: short lock that serializes the interval check of one resource.
pub fn gate_path(dir: &Path, pool: &str, resource: &str) -> PathBuf {
    dir.join(pool).join(format!("{resource}.gate.lock"))
}

/// `<dir>/<pool>/<resource>.last`: unix milliseconds of the last start on the resource.
pub fn last_path(dir: &Path, pool: &str, resource: &str) -> PathBuf {
    dir.join(pool).join(format!("{resource}.last"))
}

pub fn read_last_ms(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub fn write_last_ms(path: &Path, ms: u64) {
    let _ = fs::write(path, ms.to_string());
}

/// How long to wait so that `interval_ms` passes after `last_ms`. A last start in the future is capped at one interval.
pub fn remaining_ms(last_ms: Option<u64>, now_ms: u64, interval_ms: u64) -> u64 {
    match last_ms {
        None => 0,
        Some(l) => (l.saturating_add(interval_ms).saturating_sub(now_ms)).min(interval_ms),
    }
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
    fn remaining_math() {
        assert_eq!(remaining_ms(None, 1000, 500), 0);
        assert_eq!(remaining_ms(Some(1000), 1200, 500), 300);
        assert_eq!(remaining_ms(Some(1000), 1600, 500), 0);
        assert_eq!(remaining_ms(Some(9_999_999), 1000, 500), 500);
    }

    #[test]
    fn last_stamp_roundtrip() {
        let d = tmp("d");
        let p = last_path(&d, "p", "r");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        assert_eq!(read_last_ms(&p), None);
        write_last_ms(&p, 12345);
        assert_eq!(read_last_ms(&p), Some(12345));
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
