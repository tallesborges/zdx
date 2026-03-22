//! Audio transcription support for Telegram voice messages.

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use zdx_core::audio::transcribe;
use zdx_core::config::Config;

pub use zdx_core::audio::transcribe::{OperationCancelled, is_operation_cancelled};

/// Transcribes audio if a supported provider is configured.
///
/// Returns `Ok(None)` if no transcription provider is available.
///
/// # Errors
/// Returns an error if the operation fails.
pub async fn transcribe_audio_if_configured(
    config: &Config,
    bytes: Vec<u8>,
    filename: &str,
    mime_type: Option<&str>,
    cancel_token: Option<&CancellationToken>,
) -> Result<Option<String>> {
    transcribe::transcribe_audio_if_configured(
        config,
        &config.transcription,
        bytes,
        filename,
        mime_type,
        cancel_token,
    )
    .await
}
