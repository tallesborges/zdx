use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

use tempfile::TempDir;

static TEST_MUTEX: Mutex<()> = Mutex::new(());

/// RAII guard providing isolated `ZDX_HOME` and thread index state for unit tests.
///
/// Acquires a process-wide mutex, creates a `TempDir`, sets `ZDX_HOME`, and resets
/// `thread_index` caches. On `Drop`, it resets the cache again and restores the
/// previous `ZDX_HOME` environment variable before releasing the lock and deleting `temp`.
pub(crate) struct TestZdxHomeGuard {
    temp: TempDir,
    prev_zdx_home: Option<OsString>,
    _lock_guard: MutexGuard<'static, ()>,
}

impl TestZdxHomeGuard {
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        self.temp.path()
    }
}

impl Drop for TestZdxHomeGuard {
    fn drop(&mut self) {
        crate::core::thread_index::reset_cache_for_test();
        unsafe {
            if let Some(ref prev) = self.prev_zdx_home {
                std::env::set_var("ZDX_HOME", prev);
            } else {
                std::env::remove_var("ZDX_HOME");
            }
        }
    }
}

/// Returns an isolated RAII test environment.
pub(crate) fn temp_zdx_home() -> TestZdxHomeGuard {
    let guard = TEST_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let prev_zdx_home = std::env::var_os("ZDX_HOME");
    let temp = TempDir::new().unwrap();
    unsafe {
        std::env::set_var("ZDX_HOME", temp.path());
    }
    crate::core::thread_index::reset_cache_for_test();
    TestZdxHomeGuard {
        temp,
        prev_zdx_home,
        _lock_guard: guard,
    }
}
