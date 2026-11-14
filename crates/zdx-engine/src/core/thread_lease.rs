//! The run lease: one writer of a thread's turns at a time, across every
//! process and surface (TUI, `zdx exec`, the bot, the orchestrator service).
//!
//! A lease is an exclusive, non-blocking `flock` on
//! `$ZDX_HOME/run/thread-runs/<thread id>.lock`, held from before the turn's
//! user message is recorded until the turn is persisted. `flock` locks belong
//! to the open file, so a second acquisition in the same process conflicts
//! too. The lease is the ownership token: [`crate::core::agent::AgentOptions`]
//! carries it into the turn, and a turn without one for its thread takes its
//! own or is refused.
//!
//! This is separate from the per-append `ThreadFileLock`, which only keeps
//! single appends whole; other runs may still append delivery and posted
//! records while a lease is held.

use std::collections::HashSet;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use anyhow::{Context, Result};

use crate::config::paths;

/// Lock paths of the leases this process holds, keyed by path so tests with
/// different `ZDX_HOME`s never see each other's.
static HELD_HERE: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

/// Exclusive right to run turns on one thread. Released on drop.
#[derive(Debug)]
pub struct ThreadRunLease {
    thread_id: String,
    path: PathBuf,
    _file: File,
}

impl Drop for ThreadRunLease {
    fn drop(&mut self) {
        HELD_HERE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.path);
    }
}

/// Another holder (any process) is running a turn on the thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadBusy {
    pub thread_id: String,
}

impl std::fmt::Display for ThreadBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "thread '{}' has a turn running elsewhere; only one run may write a thread at a time",
            self.thread_id
        )
    }
}

impl std::error::Error for ThreadBusy {}

impl ThreadRunLease {
    /// Takes the lease, or fails at once with [`ThreadBusy`] when it is held.
    ///
    /// # Errors
    /// [`ThreadBusy`] when another holder has it; an I/O error when the lock
    /// file cannot be opened or locked.
    pub fn try_acquire(thread_id: &str) -> Result<Self> {
        let path = lease_path(thread_id);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {
                HELD_HERE
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(path.clone());
                Ok(Self {
                    thread_id: thread_id.to_string(),
                    path,
                    _file: file,
                })
            }
            Err(TryLockError::WouldBlock) => Err(ThreadBusy {
                thread_id: thread_id.to_string(),
            }
            .into()),
            Err(TryLockError::Error(err)) => {
                Err(err).with_context(|| format!("lock {}", path.display()))
            }
        }
    }

    #[must_use]
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Whether this process holds the thread's lease (its own turn runs).
    #[must_use]
    pub fn held_here(thread_id: &str) -> bool {
        HELD_HERE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&lease_path(thread_id))
    }
}

/// Appends `event` to `thread` unless a run in another process holds the
/// thread: a write in the middle of its turn would interleave with it. This
/// process's own turn does not block it. Returns whether it was appended.
///
/// # Errors
/// [`ThreadBusy`] when another process runs a turn; else the append error.
pub fn append_outside_foreign_turn(
    thread: &mut crate::core::thread_persistence::Thread,
    event: &crate::core::thread_persistence::ThreadEvent,
) -> Result<()> {
    if ThreadRunLease::held_here(&thread.id) {
        return thread.append(event);
    }
    let _lease = ThreadRunLease::try_acquire(&thread.id)?;
    thread.append(event)
}

/// Whether `err` is a refused lease.
#[must_use]
pub fn is_busy(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ThreadBusy>().is_some()
}

fn lease_path(thread_id: &str) -> PathBuf {
    let name: String = thread_id
        .chars()
        .map(|c| if c == '/' || c == '\0' { '_' } else { c })
        .collect();
    paths::zdx_home()
        .join("run")
        .join("thread-runs")
        .join(format!("{name}.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lease_is_refused_until_the_first_drops() {
        let _home = crate::test_support::temp_zdx_home();
        let first = ThreadRunLease::try_acquire("thread-a").unwrap();
        let err = ThreadRunLease::try_acquire("thread-a").unwrap_err();
        assert!(is_busy(&err), "{err:#}");
        assert!(ThreadRunLease::try_acquire("thread-b").is_ok());
        assert!(ThreadRunLease::held_here("thread-a"));
        drop(first);
        assert!(!ThreadRunLease::held_here("thread-a"));
        assert!(ThreadRunLease::try_acquire("thread-a").is_ok());
    }
}
