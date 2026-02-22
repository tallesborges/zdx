//! Auto thread title generation.
//!
//! Spawns a subagent to suggest a thread title from the first user message.
//! The result is written to the thread meta without emitting UI messages.

use std::path::PathBuf;

use zdx_core::core::{thread_persistence, title_generation};

use crate::events::{ThreadUiEvent, UiEvent};

/// Generates a thread title and persists it (if still unset).
///
/// Returns `UiEvent::Thread(ThreadUiEvent::TitleSuggested)` (title None on failure or skip).
pub async fn suggest_thread_title(
    thread_id: String,
    message: String,
    title_model: String,
    root: PathBuf,
) -> UiEvent {
    let title = title_generation::generate_title(&message, &title_model, &root)
        .await
        .ok();

    if title.is_none() {
        return UiEvent::Thread(ThreadUiEvent::TitleSuggested {
            thread_id,
            title: None,
        });
    }

    let title = title
        .and_then(|title| thread_persistence::set_thread_title(&thread_id, Some(title)).ok())
        .flatten();

    UiEvent::Thread(ThreadUiEvent::TitleSuggested { thread_id, title })
}
