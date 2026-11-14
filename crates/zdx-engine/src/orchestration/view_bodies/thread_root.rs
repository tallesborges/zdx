//! The thread a view request means and its stored root. The Git views
//! (here and in the shared Go `apps/shared/gitview`) look for the repository
//! in that root first, then in the views root.

use std::path::Path;

use serde::Serialize;

use super::{EngineViews, internal, invalid, resolve_alias_id, valid_thread_id};
use crate::core::{thread_index, thread_persistence};
use crate::orchestration::views::ViewError;

/// The `thread_root` view body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadRootResponse {
    /// The thread asked for, `active` resolved to the most recent Telegram
    /// thread. `None` when `active` matches no thread.
    pub thread_id: Option<String>,
    /// `thread_id` after one alias hop (a worker mirror topic resolves to
    /// its worker).
    pub effective_id: Option<String>,
    /// The effective thread's stored root, absolute (a relative one is
    /// resolved against the service's directory). `None` when it has none,
    /// or its thread file is missing or unreadable.
    pub root_path: Option<String>,
    /// The views root, absolute, where the repository is looked for when
    /// the thread's root holds none.
    pub fallback_root: String,
}

impl EngineViews {
    /// The thread `thread_id` means (`active` when absent or blank) and its
    /// stored root.
    ///
    /// # Errors
    /// `Invalid` for a bad id, `Internal` when the lookup task fails.
    pub async fn thread_root(
        &self,
        thread_id: Option<String>,
    ) -> Result<ThreadRootResponse, ViewError> {
        select_thread_root(&self.root, thread_id, "Thread root resolution failed").await
    }
}

/// Resolves the thread and its root. Read failures of the index, the alias,
/// or the root count as missing values. `failed` is the `Internal` text when
/// the blocking lookup itself fails.
pub(super) async fn select_thread_root(
    views_root: &Path,
    thread_id: Option<String>,
    failed: &'static str,
) -> Result<ThreadRootResponse, ViewError> {
    let requested_id = thread_id
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| "active".to_string());
    if requested_id != "active" && !valid_thread_id(&requested_id) {
        return Err(invalid("Invalid thread ID"));
    }

    let fallback_root = absolute(views_root);
    tokio::task::spawn_blocking(move || {
        let thread_id = if requested_id == "active" {
            thread_index::latest_thread_id_with_prefix("telegram-")
                .ok()
                .flatten()
        } else {
            Some(requested_id)
        };
        let effective_id = thread_id.as_deref().map(resolve_alias_id);
        let root_path = effective_id
            .as_deref()
            .and_then(|id| thread_persistence::read_thread_root_path(id).ok().flatten())
            .map(|root| absolute(Path::new(&root)));
        ThreadRootResponse {
            thread_id,
            effective_id,
            root_path,
            fallback_root,
        }
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "Thread root resolution task failed");
        internal(failed)
    })
}

/// `path` resolved against this process's directory, as `git -C` resolves
/// it here, so another process reads the same directory. The empty path,
/// which cannot be made absolute, is kept.
fn absolute(path: &Path) -> String {
    std::path::absolute(path)
        .as_deref()
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}
