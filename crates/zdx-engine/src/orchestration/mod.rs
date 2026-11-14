//! Shared orchestration: persistent-profile threads that delegate to worker
//! threads and continue when a worker finishes.
//!
//! One long-lived process (`zdx orchestrator serve`, the `orchestrator`
//! service) hosts the [`runtime::Orchestrator`]: it owns the worker
//! processes, the per-thread queues, and every turn of a persistent-profile
//! thread. The CLI and the Telegram bot are clients over the Unix socket in
//! [`protocol`]. Overview: `docs/ARCHITECTURE.md`, Orchestrator Service.

pub mod catalog;
pub mod client;
pub mod feedback;
pub mod helpers;
pub mod prompt;
pub mod protocol;
pub mod runtime;
pub mod server;
pub mod thread_ops;
pub mod updates;
pub mod view_bodies;
pub mod views;

use anyhow::{Result, bail};
pub use client::OrchestratorClient;
pub use runtime::{ACTIVITY_KIND, CancelTarget, Orchestrator};

/// Refuses a direct run (exec, chat, resume) on a persistent-profile thread:
/// its turns run only in the orchestrator service, the one writer of the
/// thread.
///
/// # Errors
/// Returns an error naming the command to use instead.
pub fn ensure_direct_run_allowed(thread_id: &str) -> Result<()> {
    let profile = crate::core::thread_persistence::read_persistent_profile(thread_id)
        .ok()
        .flatten();
    if let Some(profile) = profile {
        bail!(
            "thread '{thread_id}' runs the '{profile}' profile in the orchestrator service, which is its only writer; send to it with `zdx orchestrator send --thread {thread_id} -p <text>`"
        );
    }
    Ok(())
}
