//! Provider-agnostic types shared across LLM backends.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result};
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::prompts::ZDX_AGENTIC_PROMPT_TEMPLATE;
use crate::tools::ToolResult;

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
/// # Arguments
/// * `config_api_key` - Value from config file (if present)
/// * `env_var` - Environment variable name (e.g., "`OPENAI_API_KEY`")
/// * `config_section` - Config section name (e.g., "openai")
///
/// # Errors
/// Returns an error if the operation fails.
pub fn resolve_api_key(
    config_api_key: Option<&str>,
    env_var: &str,
    config_section: &str,
) -> Result<String> {
    // Try config value first
    if let Some(key) = config_api_key {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    // Fall back to env var
    std::env::var(env_var).context(format!(
        "No API key available. Set {env_var} or api_key in [providers.{config_section}]."
    ))
}

/// Resolves a base URL with precedence: env > config > default.
///
/// # Arguments
/// * `config_base_url` - Value from config file (if present)
/// * `env_var` - Environment variable name (e.g., "`OPENAI_BASE_URL`")
/// * `default_url` - Default URL if neither env nor config is set
/// * `provider_name` - Human-readable provider name for error messages
///
/// # Errors
/// Returns an error if the operation fails.
pub fn resolve_base_url(
    config_base_url: Option<&str>,
    env_var: &str,
    default_url: &str,
    provider_name: &str,
) -> Result<String> {
    // Try env var first
    if let Ok(env_url) = std::env::var(env_var) {
        let trimmed = env_url.trim();
        if !trimmed.is_empty() {
            validate_url(trimmed, provider_name)?;
            return Ok(trimmed.to_string());
        }
    }

    // Try config value
    if let Some(config_url) = config_base_url {
        let trimmed = config_url.trim();
        if !trimmed.is_empty() {
            validate_url(trimmed, provider_name)?;
            return Ok(trimmed.to_string());
        }
    }

    // Default
    Ok(default_url.to_string())
}

/// Validates that a URL is well-formed.
fn validate_url(url: &str, provider_name: &str) -> Result<()> {
    url::Url::parse(url).with_context(|| format!("Invalid {provider_name} base URL: {url}"))?;
    Ok(())
}

/// Merges the zdx agentic prompt with the provided system prompt.
///
/// Always includes the zdx agentic template, appending any caller-provided system prompt.
pub fn merge_system_prompt(system: Option<&str>) -> Option<String> {
    let base = ZDX_AGENTIC_PROMPT_TEMPLATE.trim();
    let merged = match system {
        Some(prompt) if !prompt.trim().is_empty() => {
            format!("{}\n\n{}", base, prompt.trim())
        }
        _ => base.to_string(),
    };
    Some(merged)
}

/// Provider-specific replay token for reasoning/thinking blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider")]
pub enum ReplayToken {
    /// Anthropic extended thinking - requires signature for replay
    #[serde(rename = "anthropic")]
    Anthropic { signature: String },
    /// `OpenAI` Responses API reasoning - requires id + encrypted content for cache replay
    #[serde(rename = "openai")]
    OpenAI {
        id: String,
        encrypted_content: String,
    },
    /// Gemini thought signature - required for multi-turn function calling
    #[serde(rename = "gemini")]
    Gemini { signature: String },
}

/// Provider-agnostic reasoning/thinking content with optional replay token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningBlock {
    /// Human-readable text (thinking or summary) for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Provider-specific replay data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<ReplayToken>,
}

/// Content block kinds emitted by streaming APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentBlockType {
    Text,
    ToolUse,
    Reasoning,
}

impl FromStr for ContentBlockType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "text" => Ok(Self::Text),
            "tool_use" => Ok(Self::ToolUse),
            "thinking" | "reasoning" => Ok(Self::Reasoning),
            _ => Err(format!("Unknown content block type: {value}")),
        }
    }
}

/// Content block in a chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChatContentBlock {
    /// Model reasoning/thinking content (provider-specific)
    #[serde(rename = "reasoning")]
    Reasoning(ReasoningBlock),
    #[serde(rename = "text")]
    Text(String),
    #[serde(rename = "image")]
    Image {
        /// MIME type (e.g., "image/png", "image/jpeg")
        mime_type: String,
        /// Base64-encoded image data
        data: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult(ToolResult),
}

/// Message content - either simple text or structured blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ChatContentBlock>),
}

/// A chat message with owned data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: MessageContent,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: MessageContent::Text(content.into()),
        }
    }

    /// Creates an assistant message with content blocks (for tool use).
    pub fn assistant_blocks(blocks: Vec<ChatContentBlock>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: MessageContent::Blocks(blocks),
        }
    }

    /// Creates a user message with tool results.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn tool_results(results: Vec<ToolResult>) -> Self {
        let blocks: Vec<ChatContentBlock> = results
            .into_iter()
            .map(ChatContentBlock::ToolResult)
            .collect();
        Self {
            role: "user".to_string(),
            content: MessageContent::Blocks(blocks),
        }
    }
}

/// Categories of provider errors for consistent error handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    /// HTTP status error (4xx, 5xx)
    HttpStatus,
    /// Connection timeout or request timeout
    Timeout,
    /// Failed to parse response (JSON parse error, invalid SSE, etc.)
    Parse,
    /// API-level error returned by the provider (e.g., overloaded, `rate_limit`)
    ApiError,
}

impl fmt::Display for ProviderErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderErrorKind::HttpStatus => write!(f, "http_status"),
            ProviderErrorKind::Timeout => write!(f, "timeout"),
            ProviderErrorKind::Parse => write!(f, "parse"),
            ProviderErrorKind::ApiError => write!(f, "api_error"),
        }
    }
}

/// Structured error from the provider with kind and details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderError {
    /// Error category
    pub kind: ProviderErrorKind,
    /// One-line summary suitable for display
    pub message: String,
    /// Optional additional details (e.g., raw error body)
    pub details: Option<String>,
}

impl ProviderError {
    /// Creates a new provider error.
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
        }
    }

    /// Creates a provider error with details.
    /// Creates an HTTP status error.
    pub fn http_status(status: u16, body: &str) -> Self {
        let message = format!("HTTP {status}");
        let details = if body.is_empty() {
            None
        } else {
            // Try to extract a cleaner error message from JSON
            if let Ok(json) = serde_json::from_str::<Value>(body)
                && let Some(error_obj) = json.get("error")
                && let Some(msg) = error_obj.get("message").and_then(|v| v.as_str())
            {
                return Self {
                    kind: ProviderErrorKind::HttpStatus,
                    message: format!("HTTP {status}: {msg}"),
                    details: Some(body.to_string()),
                };
            }
            Some(body.to_string())
        };
        Self {
            kind: ProviderErrorKind::HttpStatus,
            message,
            details,
        }
    }

    /// Creates a timeout error.
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Timeout, message)
    }

    /// Creates an API error (from mid-stream error event).
    pub fn api_error(error_type: &str, message: &str) -> Self {
        Self {
            kind: ProviderErrorKind::ApiError,
            message: format!("{error_type}: {message}"),
            details: None,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ProviderError {}

/// Result type for provider operations.
pub type ProviderResult<T> = std::result::Result<T, ProviderError>;

/// Token usage information from Anthropic API.
///
/// Tracks input/output tokens and cache-related tokens for cost calculation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    /// Input tokens (non-cached)
    pub input_tokens: u64,
    /// Output tokens
    pub output_tokens: u64,
    /// Tokens read from cache
    pub cache_read_input_tokens: u64,
    /// Tokens written to cache
    pub cache_creation_input_tokens: u64,
}

/// Events emitted during streaming.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Message started, contains model info and initial usage
    MessageStart { model: String, usage: Usage },
    /// A content block has started (text, `tool_use`, reasoning)
    ContentBlockStart {
        index: usize,
        block_type: ContentBlockType,
        /// For `tool_use` blocks: the tool use ID
        id: Option<String>,
        /// For `tool_use` blocks: the tool name
        name: Option<String>,
    },
    /// Text delta within a content block
    TextDelta { index: usize, text: String },
    /// Partial JSON delta for tool input
    InputJsonDelta { index: usize, partial_json: String },
    /// Reasoning delta within a reasoning content block
    ReasoningDelta { index: usize, reasoning: String },
    /// Signature delta within a reasoning content block
    ReasoningSignatureDelta { index: usize, signature: String },
    /// `OpenAI` reasoning item with encrypted content (for caching/replay)
    ReasoningCompleted {
        index: usize,
        id: String,
        encrypted_content: String,
        /// Human-readable summary of the reasoning (for display)
        summary: Option<String>,
    },
    /// A content block has ended
    ContentBlockCompleted { index: usize },
    /// Message delta (e.g., `stop_reason` update, final usage)
    MessageDelta {
        stop_reason: Option<String>,
        usage: Option<Usage>,
    },
    /// Message completed
    MessageCompleted,
    /// Ping event (keepalive)
    Ping,
    /// Error event from API
    Error { error_type: String, message: String },
}

/// Boxed stream of provider events.
pub type ProviderStream = BoxStream<'static, ProviderResult<StreamEvent>>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: `ReplayToken::Gemini` serialization round-trips correctly.
    ///
    /// Verifies that Gemini replay tokens serialize to JSON with the correct
    /// tagged format and deserialize back to the same value.
    #[test]
    fn test_replay_token_gemini_roundtrip() {
        let token = ReplayToken::Gemini {
            signature: "base64_encoded_thought_signature".to_string(),
        };

        // Serialize
        let json = serde_json::to_string(&token).unwrap();

        // Verify JSON format
        assert!(json.contains(r#""provider":"gemini""#));
        assert!(json.contains(r#""signature":"base64_encoded_thought_signature""#));

        // Deserialize
        let parsed: ReplayToken = serde_json::from_str(&json).unwrap();
        assert_eq!(token, parsed);
    }

    /// Test: `ContentBlockType` parsing for reasoning variants.
    #[test]
    fn test_content_block_type_reasoning_parsing() {
        // Both "thinking" and "reasoning" should parse to Reasoning
        assert_eq!(
            ContentBlockType::from_str("thinking").unwrap(),
            ContentBlockType::Reasoning
        );
        assert_eq!(
            ContentBlockType::from_str("reasoning").unwrap(),
            ContentBlockType::Reasoning
        );
        assert_eq!(
            ContentBlockType::from_str("text").unwrap(),
            ContentBlockType::Text
        );
        assert_eq!(
            ContentBlockType::from_str("tool_use").unwrap(),
            ContentBlockType::ToolUse
        );
    }
}
