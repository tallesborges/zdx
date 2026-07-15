pub mod speak;
pub mod transcribe;

use anyhow::{Context, Result, anyhow};
use tokio_util::sync::CancellationToken;

/// Marker error for a user-cancelled audio operation (transcription or synthesis).
#[derive(Debug)]
pub struct OperationCancelled;

impl std::fmt::Display for OperationCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "operation cancelled")
    }
}

impl std::error::Error for OperationCancelled {}

#[must_use]
pub fn is_operation_cancelled(err: &anyhow::Error) -> bool {
    err.downcast_ref::<OperationCancelled>().is_some()
}

/// Sends an HTTP request honoring cancellation, returning the response only on a
/// success status (otherwise an error carrying the response body).
///
/// Shared by the audio transcription and speech-synthesis request paths. The
/// `context` string is embedded in both the transport-failure and non-2xx
/// error messages (e.g. `"ElevenLabs transcription (url=..., model=...)"`).
pub(crate) async fn send_checked(
    request: reqwest::RequestBuilder,
    cancel_token: Option<&CancellationToken>,
    context: &str,
) -> Result<reqwest::Response> {
    if cancel_token.is_some_and(CancellationToken::is_cancelled) {
        return Err(OperationCancelled.into());
    }

    let send = request.send();
    let response = if let Some(token) = cancel_token {
        tokio::select! {
            () = token.cancelled() => return Err(OperationCancelled.into()),
            response = send => response,
        }
    } else {
        send.await
    }
    .with_context(|| format!("{context} request failed"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("{context} failed: {status} {body}"));
    }

    Ok(response)
}
