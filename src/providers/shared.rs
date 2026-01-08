//! Provider-agnostic types shared across LLM backends.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::ToolResult;

/// Content block in a chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChatContentBlock {
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "text")]
    Text(String),
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
    /// API-level error returned by the provider (e.g., overloaded, rate_limit)
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
        let message = format!("HTTP {}", status);
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
                    message: format!("HTTP {}: {}", status, msg),
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
            message: format!("{}: {}", error_type, message),
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
    /// A content block has started (text or tool_use or thinking)
    ContentBlockStart {
        index: usize,
        block_type: String,
        /// For tool_use blocks: the tool use ID
        id: Option<String>,
        /// For tool_use blocks: the tool name
        name: Option<String>,
    },
    /// Text delta within a content block
    TextDelta { index: usize, text: String },
    /// Partial JSON delta for tool input
    InputJsonDelta { index: usize, partial_json: String },
    /// Thinking delta within a thinking content block
    ThinkingDelta { index: usize, thinking: String },
    /// Signature delta within a thinking content block
    SignatureDelta { index: usize, signature: String },
    /// A content block has ended
    ContentBlockStop { index: usize },
    /// Message delta (e.g., stop_reason update, final usage)
    MessageDelta {
        stop_reason: Option<String>,
        usage: Option<Usage>,
    },
    /// Message completed
    MessageStop,
    /// Ping event (keepalive)
    Ping,
    /// Error event from API
    Error { error_type: String, message: String },
}
