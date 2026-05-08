//! Thread TLDR/recap handler.
//!
//! Spawns a subagent to summarize the most recent activity of a saved thread
//! (via `core::tldr_generation`) and emits a `UiEvent::TldrResult` so the
//! reducer can update the open TLDR overlay.

use std::path::PathBuf;

use zdx_engine::core::tldr_generation;

use crate::events::UiEvent;

/// Generates a TLDR for `thread_id` and returns a `UiEvent::TldrResult`.
pub async fn generate_tldr(thread_id: String, tldr_model: String, root: PathBuf) -> UiEvent {
    let result = tldr_generation::generate_tldr(&thread_id, &tldr_model, &root)
        .await
        .map_err(|err| err.to_string());
    UiEvent::TldrResult { thread_id, result }
}
