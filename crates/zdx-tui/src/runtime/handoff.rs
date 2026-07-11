//! Handoff generation handlers.
//!
//! Thin TUI adapter over `zdx_engine::core::handoff_generation`: runs the
//! shared engine generation and wraps the outcome in a `UiEvent`.
//!
//! Uses `CancellationToken` for the unified cancellation model.

use std::path::PathBuf;

use tokio_util::sync::CancellationToken;
use zdx_engine::core::handoff_generation::generate_handoff;

use crate::events::UiEvent;

/// Runs handoff generation with cancellation support.
///
/// Returns `UiEvent::HandoffResult`; cancellation is cooperative via token.
pub async fn handoff_generation(
    thread_id: String,
    next_message: String,
    handoff_model: String,
    root: PathBuf,
    cancel: Option<CancellationToken>,
) -> UiEvent {
    let result = generate_handoff(&thread_id, &next_message, &handoff_model, &root, cancel)
        .await
        .map_err(|err| format!("{err:#}"));
    UiEvent::HandoffResult {
        next_message,
        result,
    }
}
