//! Provider-facing error, usage, and streaming value types.

use std::fmt;

use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::messages::{ContentBlockType, IdOrigin, SignatureProvider};

/// Categories of provider errors for consistent error handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    /// Transport-level request or stream failure
    Transport,
    /// Request construction or redirect failure
    Request,
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
            ProviderErrorKind::Transport => write!(f, "transport"),
            ProviderErrorKind::Request => write!(f, "request"),
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
    /// HTTP response status when available
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Provider-native error code or type when available
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
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

const TERMINAL_PATTERNS: &[&str] = &[
    "insufficient_quota",
    "quota exceeded",
    "out of budget",
    "billing",
    "monthly usage limit",
    "weekly usage limit",
    "daily usage limit",
    "free usage limit",
    "usage limit reached",
    "available balance",
    "invalid api key",
    "invalid_api_key",
];

const TERMINAL_CODES: &[&str] = &[
    "insufficient_quota",
    "billing_error",
    "billing_not_active",
    "usage_limit",
    "usage_limit_reached",
    "monthly_usage_limit",
    "free_usage_limit",
    "invalid_api_key",
    "authentication_error",
    "permission_error",
    "invalid_request",
    "invalid_request_error",
];

const TRANSIENT_CODES: &[&str] = &[
    "overloaded",
    "overloaded_error",
    "rate_limit",
    "rate_limit_error",
    "resource_exhausted",
    "service_unavailable",
    "server_error",
    "internal_error",
    "temporarily_unavailable",
];

impl ProviderError {
    /// Creates a new provider error.
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            code: None,
            message: message.into(),
            details: None,
        }
    }

    /// Creates an HTTP status error.
    pub fn http_status(status: u16, body: &str) -> Self {
        let parsed = serde_json::from_str::<Value>(body).ok();
        let code = parsed.as_ref().and_then(extract_error_code);
        let provider_message = parsed.as_ref().and_then(extract_error_message);
        let message = provider_message.map_or_else(
            || format!("HTTP {status}"),
            |message| format!("HTTP {status}: {message}"),
        );
        let details = (!body.is_empty()).then(|| body.to_string());
        Self {
            kind: ProviderErrorKind::HttpStatus,
            status: Some(status),
            code,
            message,
            details,
        }
    }

    /// Creates a timeout error.
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Timeout, message)
    }

    /// Creates a transport error.
    pub fn transport(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Transport, message)
    }

    /// Creates a request-construction error.
    pub fn request(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Request, message)
    }

    /// Creates an API error (from mid-stream error event).
    pub fn api_error(error_type: &str, message: &str) -> Self {
        Self {
            kind: ProviderErrorKind::ApiError,
            status: None,
            code: Some(error_type.to_string()),
            message: format!("{error_type}: {message}"),
            details: None,
        }
    }

    /// Returns true if this error is transient and safe to retry automatically.
    pub fn is_retryable(&self) -> bool {
        match self.kind {
            ProviderErrorKind::Transport | ProviderErrorKind::Timeout => return true,
            ProviderErrorKind::Request | ProviderErrorKind::Parse => return false,
            ProviderErrorKind::HttpStatus | ProviderErrorKind::ApiError => {}
        }

        let normalized_code = self.code.as_deref().map(normalize_code);
        let haystack = format!(
            "{} {}",
            self.message.to_lowercase(),
            self.details.as_deref().unwrap_or("").to_lowercase()
        );

        if normalized_code
            .as_deref()
            .is_some_and(|code| TERMINAL_CODES.contains(&code))
            || TERMINAL_PATTERNS
                .iter()
                .any(|pattern| haystack.contains(pattern))
        {
            return false;
        }

        if matches!(self.kind, ProviderErrorKind::HttpStatus)
            && let Some(status) = self.status
            && (status == 408 || status == 429 || (500..=599).contains(&status))
        {
            return true;
        }

        if normalized_code
            .as_deref()
            .is_some_and(|code| TRANSIENT_CODES.contains(&code))
        {
            return true;
        }

        if matches!(self.kind, ProviderErrorKind::HttpStatus) && self.status.is_some() {
            return false;
        }

        RETRYABLE_PATTERNS.iter().any(|p| haystack.contains(p))
    }
}

fn extract_error_code(payload: &Value) -> Option<String> {
    let error = payload.get("error").unwrap_or(payload);
    ["code", "type", "status"].iter().find_map(|key| {
        error
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn extract_error_message(payload: &Value) -> Option<&str> {
    payload
        .get("error")
        .unwrap_or(payload)
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
}

fn normalize_code(code: &str) -> String {
    code.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|character| match character {
            '-' | ' ' => '_',
            other => other,
        })
        .collect()
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
        /// For `tool_use` blocks: whether the `id` was emitted by the
        /// provider (`Real`) or synthesized locally because the provider
        /// omitted one (`Synthesized`). `None` for non-`tool_use` blocks
        /// and for providers that do not distinguish (Anthropic / `OpenAI`).
        /// Used by the Gemini request builder to decide whether to replay
        /// the id on the wire — see `IdOrigin` docs.
        id_origin: Option<IdOrigin>,
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
    /// A content block has ended.
    ///
    /// `signature` carries a per-part replay signature when the provider
    /// emits one (currently: Gemini's `thoughtSignature` on text and
    /// `tool_use` parts). Anthropic and `OpenAI` signatures continue to flow
    /// through their dedicated events (`ReasoningSignatureDelta` /
    /// `ReasoningCompleted`); this field exists for the cross-provider case
    /// where no dedicated signature event applies.
    ContentBlockCompleted {
        index: usize,
        signature: Option<(SignatureProvider, String)>,
    },
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
        assert_eq!(err.code.as_deref(), Some("overloaded_error"));
        assert!(err.is_retryable());
    }

    #[test]
    fn test_retryable_rate_limit() {
        assert!(ProviderError::api_error("rate_limit_error", "Rate limit").is_retryable());
        assert!(ProviderError::http_status(429, "").is_retryable());
    }

    #[test]
    fn test_structured_http_status_classification() {
        for status in [408, 429, 500, 502, 503, 504, 529, 599] {
            let err = ProviderError::http_status(status, "");
            assert!(err.is_retryable(), "expected retryable HTTP {status}");
        }
        for status in [400, 401, 403, 404, 422] {
            let err = ProviderError::http_status(status, "");
            assert!(!err.is_retryable(), "expected terminal HTTP {status}");
        }
    }

    #[test]
    fn test_http_status_extracts_common_provider_codes() {
        let openai = ProviderError::http_status(
            429,
            r#"{"error":{"message":"quota exhausted","type":"insufficient_quota"}}"#,
        );
        assert_eq!(openai.status, Some(429));
        assert_eq!(openai.code.as_deref(), Some("insufficient_quota"));
        assert!(openai.message.contains("quota exhausted"));

        let anthropic = ProviderError::http_status(
            529,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#,
        );
        assert_eq!(anthropic.code.as_deref(), Some("overloaded_error"));
        assert!(anthropic.is_retryable());

        let gemini = ProviderError::http_status(
            429,
            r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"retry later"}}"#,
        );
        assert_eq!(gemini.code.as_deref(), Some("RESOURCE_EXHAUSTED"));
        assert!(gemini.is_retryable());
    }

    #[test]
    fn test_terminal_provider_evidence_overrides_retryable_status_or_text() {
        let quota = ProviderError::http_status(
            429,
            r#"{"error":{"type":"insufficient_quota","message":"server error"}}"#,
        );
        assert!(!quota.is_retryable());

        let billing = ProviderError::http_status(
            503,
            r#"{"error":{"message":"Billing account is not active"}}"#,
        );
        assert!(!billing.is_retryable());

        let invalid = ProviderError::api_error("invalid_request_error", "internal error");
        assert!(!invalid.is_retryable());
    }

    #[test]
    fn test_transient_provider_code_overrides_otherwise_terminal_http_status() {
        let overloaded = ProviderError::http_status(
            400,
            r#"{"error":{"type":"overloaded_error","message":"busy"}}"#,
        );
        assert!(overloaded.is_retryable());
    }

    #[test]
    fn test_unknown_unstructured_errors_use_text_fallback() {
        assert!(
            ProviderError::new(ProviderErrorKind::ApiError, "upstream connect failed")
                .is_retryable()
        );
        assert!(!ProviderError::new(ProviderErrorKind::ApiError, "bad model").is_retryable());
    }

    #[test]
    fn test_provider_error_structured_metadata_serialization() {
        let structured = ProviderError::http_status(
            529,
            r#"{"error":{"type":"overloaded_error","message":"busy"}}"#,
        );
        let json = serde_json::to_value(&structured).unwrap();
        assert_eq!(json["status"], 529);
        assert_eq!(json["code"], "overloaded_error");

        let plain = serde_json::to_value(ProviderError::transport("disconnected")).unwrap();
        assert!(plain.get("status").is_none());
        assert!(plain.get("code").is_none());

        let restored: ProviderError = serde_json::from_value(serde_json::json!({
            "kind": "transport",
            "message": "legacy",
            "details": null
        }))
        .unwrap();
        assert_eq!(restored.status, None);
        assert_eq!(restored.code, None);
    }

    #[test]
    fn test_retryable_timeout_and_network() {
        assert!(ProviderError::timeout("Connection timed out").is_retryable());
        assert!(ProviderError::transport("error sending request for url").is_retryable());
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
        assert!(!ProviderError::request("builder error").is_retryable());
        assert!(!ProviderError::new(ProviderErrorKind::Parse, "Invalid JSON").is_retryable());
        assert!(!ProviderError::api_error("invalid_request", "Bad model").is_retryable());
    }
}
