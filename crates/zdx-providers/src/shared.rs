//! Provider-agnostic shared helpers. Pure value types live in `zdx_types`.

use anyhow::{Context, Result};
use serde_json::Value;
pub use zdx_types::{
    ChatContentBlock, ChatMessage, ContentBlockType, MessageContent, ProviderError,
    ProviderErrorKind, ProviderResult, ProviderStream, ReasoningBlock, ReplayToken,
    SignatureProvider, StreamEvent, Usage,
};

/// Standard User-Agent header for zdx API requests.
///
/// Used by all API-key providers for identification. OAuth providers
/// (Claude CLI, Gemini CLI) use mimicked User-Agents for compatibility.
pub const USER_AGENT: &str = concat!("zdx/", env!("CARGO_PKG_VERSION"));

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
}
