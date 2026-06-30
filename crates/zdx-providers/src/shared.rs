//! Provider-agnostic shared helpers. Pure value types live in `zdx_types`.

use anyhow::{Context, Result};
use eventsource_stream::EventStreamError;
use reqwest::header::HeaderValue;
use serde_json::Value;
pub use zdx_types::providers::UsageDelta;
pub use zdx_types::{
    ChatContentBlock, ChatMessage, ContentBlockType, IdOrigin, MessageContent, ProviderError,
    ProviderErrorKind, ProviderResult, ProviderStream, ReasoningBlock, ReplayToken,
    SignatureProvider, StreamEvent, Usage,
};

/// Standard User-Agent header for zdx API requests.
///
/// Used by all API-key providers for identification. OAuth providers
/// (Claude CLI, Gemini CLI) use mimicked User-Agents for compatibility.
pub const USER_AGENT: &str = concat!("zdx/", env!("CARGO_PKG_VERSION"));

/// Parses a string into an HTTP header value, returning a contextual error
/// instead of silently substituting an empty value when the input contains
/// bytes that are invalid in an HTTP header (e.g. a stray newline or control
/// character in an API key or token). The error never includes the value.
///
/// # Errors
/// Returns an error if `value` is not a valid HTTP header value.
pub(crate) fn header_value(label: &str, value: &str) -> Result<HeaderValue> {
    HeaderValue::from_str(value)
        .with_context(|| format!("{label} is not a valid HTTP header value"))
}

// ============================================================================
// Config resolution helpers
// ============================================================================

/// Resolves an API key with precedence: config > env.
///
/// # Errors
/// Returns an error if no key is available in config or env.
pub fn resolve_api_key(
    config_api_key: Option<&str>,
    env_var: &str,
    config_section: &str,
) -> Result<String> {
    if let Some(key) = config_api_key {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    std::env::var(env_var).context(format!(
        "No API key available. Set {env_var} or api_key in [providers.{config_section}]."
    ))
}

/// Resolves a base URL with precedence: env > config > default.
///
/// # Errors
/// Returns an error if the configured URL is not parseable.
pub fn resolve_base_url(
    config_base_url: Option<&str>,
    env_var: &str,
    default_url: &str,
    provider_name: &str,
) -> Result<String> {
    if let Ok(env_url) = std::env::var(env_var) {
        let trimmed = env_url.trim();
        if !trimmed.is_empty() {
            validate_url(trimmed, provider_name)?;
            return Ok(trimmed.to_string());
        }
    }

    if let Some(config_url) = config_base_url {
        let trimmed = config_url.trim();
        if !trimmed.is_empty() {
            validate_url(trimmed, provider_name)?;
            return Ok(trimmed.to_string());
        }
    }

    Ok(default_url.to_string())
}

/// Validates that a URL is well-formed.
fn validate_url(url: &str, provider_name: &str) -> Result<()> {
    url::Url::parse(url).with_context(|| format!("Invalid {provider_name} base URL: {url}"))?;
    Ok(())
}

/// Normalizes the provided system prompt.
///
/// Returns `None` when the prompt is absent or whitespace-only.
pub fn merge_system_prompt(system: Option<&str>) -> Option<String> {
    system
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .map(ToOwned::to_owned)
}

/// Extracts a user-visible provider error message from a JSON error payload.
///
/// Falls back to a compact serialized payload when the provider omits a
/// top-level string message.
#[must_use]
pub fn error_message_from_payload(error: &Value, message_keys: &[&str]) -> String {
    for key in message_keys {
        if let Some(message) = error.get(key).and_then(Value::as_str) {
            let trimmed = message.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }

    let raw = serde_json::to_string(error).unwrap_or_else(|_| format!("{error:?}"));
    let compact = if raw.len() > 1200 {
        format!("{}…", &raw[..1200])
    } else {
        raw
    };
    format!("Upstream error payload: {compact}")
}

/// Maps an `EventStreamError` from the `eventsource-stream` SSE parser into a
/// `ProviderError`, distinguishing transport-level failures (retryable) from
/// SSE framing/UTF-8 parser failures (non-retryable).
///
/// Transport failures (socket reset, connection dropped, etc.) are classified
/// as `ProviderErrorKind::Timeout` with an explicit `"network error"` token
/// so `ProviderError::is_retryable()` matches them via `RETRYABLE_PATTERNS`
/// and the engine's transparent retry loop can recover before any visible
/// output. UTF-8 / SSE-parser errors stay as `ProviderErrorKind::Parse` and
/// remain non-retryable, matching the contract that real protocol/decoding
/// bugs should surface as fatal turn failures rather than auto-retried.
pub fn map_event_stream_error<E>(err: EventStreamError<E>) -> ProviderError
where
    E: std::fmt::Display,
{
    match err {
        EventStreamError::Transport(e) => ProviderError::new(
            ProviderErrorKind::Timeout,
            format!("SSE stream network error: {e}"),
        ),
        EventStreamError::Utf8(e) => ProviderError::new(
            ProviderErrorKind::Parse,
            format!("SSE stream UTF-8 error: {e}"),
        ),
        EventStreamError::Parser(e) => ProviderError::new(
            ProviderErrorKind::Parse,
            format!("SSE stream parse error: {e}"),
        ),
    }
}

/// Extracts text and an optional image from tool result content.
///
/// Returns the text output and `Some((mime_type, base64_data))` when the
/// result carries an image block.
pub(crate) fn extract_tool_result_with_image(
    content: &zdx_types::ToolResultContent,
) -> (String, Option<(String, String)>) {
    match content {
        zdx_types::ToolResultContent::Text(text) => (text.clone(), None),
        zdx_types::ToolResultContent::Blocks(blocks) => {
            let text = blocks
                .iter()
                .find_map(|block| match block {
                    zdx_types::ToolResultBlock::Text { text } => Some(text.clone()),
                    zdx_types::ToolResultBlock::Image { .. } => None,
                })
                .unwrap_or_default();

            let image = blocks.iter().find_map(|block| match block {
                zdx_types::ToolResultBlock::Image { mime_type, data } => {
                    Some((mime_type.clone(), data.clone()))
                }
                zdx_types::ToolResultBlock::Text { .. } => None,
            });

            (text, image)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_system_prompt_passthrough() {
        assert_eq!(merge_system_prompt(None), None);
        assert_eq!(merge_system_prompt(Some("   ")), None);
        assert_eq!(
            merge_system_prompt(Some("  hello world  ")),
            Some("hello world".to_string())
        );
    }

    #[test]
    fn test_map_event_stream_error_transport_is_retryable() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "broken pipe");
        let err: EventStreamError<std::io::Error> = EventStreamError::Transport(io_err);
        let mapped = map_event_stream_error(err);

        assert_eq!(mapped.kind, ProviderErrorKind::Timeout);
        assert!(mapped.message.contains("network error"));
        assert!(
            mapped.is_retryable(),
            "transport-level SSE failures must be retryable, got {mapped:?}",
        );
    }

    #[test]
    fn test_map_event_stream_error_utf8_is_not_retryable() {
        let utf8_err = String::from_utf8(vec![0xF0, 0x9F]).unwrap_err();
        let err: EventStreamError<std::io::Error> = EventStreamError::Utf8(utf8_err);
        let mapped = map_event_stream_error(err);

        assert_eq!(mapped.kind, ProviderErrorKind::Parse);
        assert!(
            !mapped.is_retryable(),
            "UTF-8 framing errors must stay non-retryable, got {mapped:?}",
        );
    }
}
