//! Provider-facing error, usage, and streaming value types.

use std::fmt;

use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::messages::{ContentBlockType, SignatureProvider};

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

/// Substrings used by `ProviderError::is_retryable` to classify transient failures.
const RETRYABLE_PATTERNS: &[&str] = &[
    "overloaded",
    "provider returned error",
    "rate limit",
    "rate_limit",
    "too many requests",
    "429",
    "500",
    "502",
    "503",
    "504",
    "service unavailable",
    "server error",
    "internal error",
    "network error",
    "connection error",
    "connection refused",
    "other side closed",
    "fetch failed",
    "upstream connect",
    "reset before headers",
    "socket hang up",
    "ended without",
    "timed out",
    "timeout",
    "terminated",
    "retry delay",
];

impl ProviderError {
    /// Creates a new provider error.
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
        }
    }

    /// Creates an HTTP status error.
    pub fn http_status(status: u16, body: &str) -> Self {
        let message = format!("HTTP {status}");
        let details = if body.is_empty() {
            None
        } else {
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

    /// Returns true if this error is transient and safe to retry automatically.
    pub fn is_retryable(&self) -> bool {
        if matches!(self.kind, ProviderErrorKind::Parse) {
            return false;
        }
        let haystack = format!(
            "{} {}",
            self.message.to_lowercase(),
            self.details.as_deref().unwrap_or("").to_lowercase()
        );
        RETRYABLE_PATTERNS.iter().any(|p| haystack.contains(p))
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

impl Usage {
    /// Returns true when every usage counter is zero.
    pub fn is_empty(&self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.cache_read_input_tokens == 0
            && self.cache_creation_input_tokens == 0
    }
}

/// Possibly-partial cumulative usage update from a streaming `message_delta`.
/// Missing fields mean "unchanged", not zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageDelta {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

impl From<Usage> for UsageDelta {
    fn from(value: Usage) -> Self {
        Self {
            input_tokens: Some(value.input_tokens),
            output_tokens: Some(value.output_tokens),
            cache_read_input_tokens: Some(value.cache_read_input_tokens),
            cache_creation_input_tokens: Some(value.cache_creation_input_tokens),
        }
    }
}

impl UsageDelta {
    /// Computes the incremental usage represented by this sparse cumulative
    /// update relative to the previously seen cumulative totals.
    pub fn incremental_from(&self, previous: &Usage) -> Usage {
        Usage {
            input_tokens: self
                .input_tokens
                .unwrap_or(previous.input_tokens)
                .saturating_sub(previous.input_tokens),
            output_tokens: self
                .output_tokens
                .unwrap_or(previous.output_tokens)
                .saturating_sub(previous.output_tokens),
            cache_read_input_tokens: self
                .cache_read_input_tokens
                .unwrap_or(previous.cache_read_input_tokens)
                .saturating_sub(previous.cache_read_input_tokens),
            cache_creation_input_tokens: self
                .cache_creation_input_tokens
                .unwrap_or(previous.cache_creation_input_tokens)
                .saturating_sub(previous.cache_creation_input_tokens),
        }
    }

    /// Applies this sparse cumulative update onto the previously seen totals.
    pub fn apply_to(&self, previous: &mut Usage) {
        previous.input_tokens = self.input_tokens.unwrap_or(previous.input_tokens);
        previous.output_tokens = self.output_tokens.unwrap_or(previous.output_tokens);
        previous.cache_read_input_tokens = self
            .cache_read_input_tokens
            .unwrap_or(previous.cache_read_input_tokens);
        previous.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .unwrap_or(previous.cache_creation_input_tokens);
    }
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
        /// For `redacted_thinking` blocks: the opaque encrypted payload
        /// that must be replayed back to the provider unchanged on
        /// subsequent turns. `None` for every other block type.
        data: Option<String>,
    },
    /// Text delta within a content block
    TextDelta { index: usize, text: String },
    /// Partial JSON delta for tool input
    InputJsonDelta { index: usize, partial_json: String },
    /// Reasoning delta within a reasoning content block
    ReasoningDelta { index: usize, reasoning: String },
    /// Signature delta within a reasoning content block
    ReasoningSignatureDelta {
        index: usize,
        signature: String,
        provider: SignatureProvider,
    },
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
        usage: Option<UsageDelta>,
    },
    /// Message completed
    MessageCompleted,
    /// Provider event intentionally ignored because it carries metadata or an
    /// unsupported block/delta shape that ZDX does not currently render.
    Ignored { kind: String },
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

    #[test]
    fn test_retryable_overloaded() {
        let err = ProviderError::api_error("overloaded_error", "API is temporarily overloaded");
        assert!(err.is_retryable());
    }

    #[test]
    fn test_retryable_rate_limit() {
        assert!(ProviderError::api_error("rate_limit_error", "Rate limit").is_retryable());
        assert!(ProviderError::new(ProviderErrorKind::HttpStatus, "HTTP 429").is_retryable());
    }

    #[test]
    fn test_retryable_http_5xx() {
        for code in ["HTTP 500", "HTTP 502", "HTTP 503", "HTTP 504"] {
            let err = ProviderError::new(ProviderErrorKind::HttpStatus, code);
            assert!(err.is_retryable(), "expected retryable: {code}");
        }
    }

    #[test]
    fn test_retryable_timeout_and_network() {
        assert!(ProviderError::timeout("Connection timed out").is_retryable());
        assert!(
            ProviderError::new(
                ProviderErrorKind::HttpStatus,
                "fetch failed: socket hang up"
            )
            .is_retryable()
        );
    }

    #[test]
    fn test_not_retryable() {
        assert!(!ProviderError::new(ProviderErrorKind::Parse, "Invalid JSON").is_retryable());
        assert!(!ProviderError::api_error("invalid_request", "Bad model").is_retryable());
        assert!(!ProviderError::new(ProviderErrorKind::HttpStatus, "HTTP 400").is_retryable());
        assert!(!ProviderError::new(ProviderErrorKind::HttpStatus, "HTTP 401").is_retryable());
    }
}
