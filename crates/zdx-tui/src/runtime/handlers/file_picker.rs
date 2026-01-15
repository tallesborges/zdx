use std::path::PathBuf;

use tokio_util::sync::CancellationToken;

use crate::events::UiEvent;

/// File discovery with cancellation support.
///
/// Returns a result event directly; cancellation is cooperative via token.
pub async fn file_discovery(root: PathBuf, cancel: Option<CancellationToken>) -> UiEvent {
    use crate::overlays::discover_files;

    let cancel = cancel.unwrap_or_default();
    let cancel_clone = cancel.clone();
    let files = tokio::task::spawn_blocking(move || discover_files(&root, &cancel_clone))
        .await
        .unwrap_or_default();
    UiEvent::FilesDiscovered(files)
}
