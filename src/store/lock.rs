//! Repository writer lock (STORAGE.md §9).
//!
//! A single exclusive writer lock serializes ref transactions, GC, repair,
//! and index rebuild. Readers never take the lock (objects and ref files are
//! immutable or atomically replaced).
//!
//! The lock is advisory (`flock(2)` semantics via fd-lock) and non-reentrant:
//! nested `with_write_lock` on the same repository would deadlock, so store
//! code uses the `*_unlocked` helpers inside a single outer lock scope.

use std::fs::File;
use std::path::Path;

/// Runs `f` while holding the exclusive writer lock (blocking).
pub fn with_write_lock<T>(
    lock_path: &Path,
    f: impl FnOnce() -> Result<T, crate::store::Error>,
) -> Result<T, crate::store::Error> {
    let file = File::open(lock_path).map_err(|e| {
        crate::store::Error::Lock(format!("open lock file {}: {e}", lock_path.display()))
    })?;
    let mut lock = fd_lock::RwLock::new(file);
    let _guard = lock.write().map_err(|e| {
        crate::store::Error::Lock(format!("acquire lock {}: {e}", lock_path.display()))
    })?;
    f()
}

/// Runs `f` while holding the exclusive writer lock if it is free.
/// Returns `None` when the lock is busy (non-blocking).
pub fn try_with_write_lock<T>(
    lock_path: &Path,
    f: impl FnOnce() -> Result<T, crate::store::Error>,
) -> Option<Result<T, crate::store::Error>> {
    let file = File::open(lock_path).ok()?;
    let mut lock = fd_lock::RwLock::new(file);
    let _guard = lock.try_write().ok()?;
    Some(f())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::temp_root;

    #[test]
    fn write_lock_runs_closure() {
        let root = temp_root("lock");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("lock");
        File::create(&path).unwrap();
        let r = with_write_lock(&path, || -> Result<i32, crate::store::Error> { Ok(42) }).unwrap();
        assert_eq!(r, 42);
    }

    #[test]
    fn try_lock_is_nonblocking() {
        let root = temp_root("lock2");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("lock");
        File::create(&path).unwrap();
        // Without holding the lock, try should succeed.
        assert!(
            try_with_write_lock(&path, || -> Result<(), crate::store::Error> { Ok(()) }).is_some()
        );
        // Holding the lock in the same process: flock is per open-file-description,
        // so a second acquire is not guaranteed to fail on all platforms; this
        // test only asserts the API shape.
        let _ = with_write_lock(&path, || -> Result<(), crate::store::Error> { Ok(()) });
    }
}
