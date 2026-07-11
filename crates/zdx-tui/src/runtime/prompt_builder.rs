//! Prompt-builder generation handlers.
//!
//! Thin TUI adapter over `zdx_engine::core::prompt_builder_generation`: runs
//! the shared engine generation and wraps the outcome in a `UiEvent`.
//!
//! Uses `CancellationToken` for the unified cancellation model.

use std::path::PathBuf;

use tokio_util::sync::CancellationToken;
use zdx_engine::core::prompt_builder_generation::generate_prompt_builder;

use crate::events::UiEvent;

/// Runs prompt-builder generation with cancellation support.
///
/// Returns `UiEvent::PromptBuilderResult`; cancellation is cooperative via the
/// supplied token.
pub async fn prompt_builder_generation(
    intent: String,
    model: Option<String>,
    root: PathBuf,
    cancel: Option<CancellationToken>,
) -> UiEvent {
    let result = generate_prompt_builder(&intent, model, &root, cancel)
        .await
        .map_err(|err| format!("{err:#}"));
    UiEvent::PromptBuilderResult { intent, result }
}
