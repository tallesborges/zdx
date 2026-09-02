//! Per-path mutual exclusion for file-mutating tools.
//!
//! Tool calls in one turn run concurrently. Two `Edit`/`Write`/`apply_patch`
//! calls on the same file would otherwise interleave their read→write steps and
//! silently drop one of the changes. Callers hold a lock for the canonical path
//! for the whole read→modify→write span; different files still run in parallel.
//! The lock is in-process only: separate `zdx` processes are not coordinated.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Returns the lock for `path`. Callers `.lock()` the returned mutex and hold
/// the guard across their read→write.
///
/// The key is the canonical path when the file exists, otherwise the canonical
/// parent joined with the file name, so relative spellings, `..` segments, and
/// symlinks all map to the same lock.
pub fn for_path(path: &Path) -> Arc<Mutex<()>> {
    let key = canonical_key(path);
    let mut locks = LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(locks.entry(key).or_default())
}

fn canonical_key(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(parent) = parent.canonicalize()
    {
        return parent.join(name);
    }
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn same_file_different_spellings_share_a_lock() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("sub")).unwrap();
        let file = temp.path().join("a.txt");
        fs::write(&file, "x").unwrap();

        let direct = for_path(&file);
        let via_dotdot = for_path(&temp.path().join("sub/../a.txt"));
        assert!(Arc::ptr_eq(&direct, &via_dotdot));
    }

    #[test]
    fn missing_file_keys_on_canonical_parent() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("sub")).unwrap();

        let a = for_path(&temp.path().join("new.txt"));
        let b = for_path(&temp.path().join("sub/../new.txt"));
        assert!(Arc::ptr_eq(&a, &b));

        let other = for_path(&temp.path().join("other.txt"));
        assert!(!Arc::ptr_eq(&a, &other));
    }
}
