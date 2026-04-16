//! Agent event and tool output value types.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::messages::{ChatMessage, ReasoningBlock};
use crate::providers::ProviderErrorKind;

/// Events emitted by the agent during execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Turn has started processing.
    TurnStarted,

    /// Incremental reasoning chunk from the assistant (extended thinking).
    ReasoningDelta { text: String },

    /// Complete reasoning block from the assistant (persisted in thread logs).
    ReasoningCompleted { block: ReasoningBlock },

    /// Incremental text chunk from the assistant.
    AssistantDelta { text: String },

    /// Complete response from the assistant.
    AssistantCompleted { text: String },

    /// Model has decided to call a tool (emitted early for UI feedback).
    /// The input may be empty at this point; `ToolInputCompleted` follows with full input.
    ToolRequested {
        id: String,
        name: String,
        input: Value,
    },

    /// Tool input JSON is fully received (for thread persistence).
    /// Emitted after `ToolRequested` once all input JSON has been streamed.
    ToolInputCompleted {
        id: String,
        name: String,
        input: Value,
    },

    /// Incremental tool input preview derived from streaming JSON.
    /// Used for UI streaming updates (not persisted).
    ToolInputDelta {
        id: String,
        name: String,
        delta: String,
    },

    /// A tool invocation has started execution.
    ToolStarted { id: String, name: String },

    /// Incremental output from a running tool (stdout/stderr).
    ToolOutputDelta { id: String, chunk: String },

    /// A tool invocation has completed.
    ToolCompleted { id: String, result: ToolOutput },

    /// An error occurred during execution.
    Error {
        /// Error category for structured handling
        kind: ErrorKind,
        /// One-line summary
        message: String,
        /// Optional additional details
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<String>,
    },

    /// A non-fatal informational notice from the model or runtime.
    /// The turn still completes; this is purely informational so the UI
    /// can surface what happened (e.g. the model declined the request,
    /// or generation stopped due to context window exhaustion).
    Notice {
        /// Notice category for structured handling.
        kind: NoticeKind,
        /// One-line human-readable summary.
        message: String,
        /// Optional additional details.
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<String>,
    },

    /// A transient provider failure was hit and the agent is backing off
    /// before retrying the request.
    ProviderRetry {
        /// Provider error category carried from the failed attempt.
        kind: ErrorKind,
        /// One-line human-readable summary of the provider error.
        message: String,
        /// Optional raw error body/details from the provider.
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<String>,
        /// 1-indexed retry attempt about to be performed.
        attempt: u32,
        /// Total number of retry attempts that will be performed.
        max_retries: u32,
        /// Backoff delay before this attempt (milliseconds).
        delay_ms: u64,
    },

    /// Turn reached a terminal state with the latest text and message snapshot.
    TurnFinished {
        /// Terminal status for the turn.
        status: TurnStatus,
        /// Final or partial accumulated text from the assistant.
        final_text: String,
        /// Updated message history (includes assistant responses and tool results).
        messages: Vec<ChatMessage>,
    },

    /// Token usage update from the provider.
    UsageUpdate {
        /// Input tokens (non-cached)
        input_tokens: u64,
        /// Output tokens
        output_tokens: u64,
        /// Tokens read from cache
        cache_read_input_tokens: u64,
        /// Tokens written to cache
        cache_creation_input_tokens: u64,
    },
}

/// Error categories for `AgentEvent::Error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// HTTP status error (4xx, 5xx)
    HttpStatus,
    /// Connection/request timeout
    Timeout,
    /// Response parsing failed
    Parse,
    /// API-level error from provider
    ApiError,
    /// Internal/unknown error
    Internal,
}

/// Notice categories for `AgentEvent::Notice`.
///
/// These are informational, not errors — the turn still completes. They
/// surface model/runtime conditions the user should know about (e.g. a
/// `refusal` or `model_context_window_exceeded` stop reason from Claude
/// 4.5+).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// The model declined to respond (Anthropic `stop_reason=refusal`).
    Refusal,
    /// Generation stopped due to context window exhaustion
    /// (Anthropic `stop_reason=model_context_window_exceeded`).
    ContextWindowExceeded,
}

/// Terminal status for a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed {
        kind: ErrorKind,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<String>,
    },
}

impl From<ProviderErrorKind> for ErrorKind {
    fn from(kind: ProviderErrorKind) -> Self {
        match kind {
            ProviderErrorKind::HttpStatus => ErrorKind::HttpStatus,
            ProviderErrorKind::Timeout => ErrorKind::Timeout,
            ProviderErrorKind::Parse => ErrorKind::Parse,
            ProviderErrorKind::ApiError => ErrorKind::ApiError,
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::HttpStatus => write!(f, "http_status"),
            ErrorKind::Timeout => write!(f, "timeout"),
            ErrorKind::Parse => write!(f, "parse"),
            ErrorKind::ApiError => write!(f, "api_error"),
            ErrorKind::Internal => write!(f, "internal"),
        }
    }
}

/// Structured envelope for tool outputs (per SPEC §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutput {
    Success {
        ok: bool,
        data: Value,
        /// Optional image content (not serialized to JSON).
        image: Option<ImageContent>,
    },
    Failure {
        ok: bool,
        error: ToolError,
    },
    /// User canceled tool execution (serializes as failure with code="canceled").
    Canceled {
        /// User-facing message.
        message: String,
    },
}

/// Special error code that indicates a canceled operation.
const CANCELED_ERROR_CODE: &str = "canceled";

impl Serialize for ToolOutput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        match self {
            ToolOutput::Success { ok, data, .. } => {
                let mut state = serializer.serialize_struct("ToolOutput", 2)?;
                state.serialize_field("ok", ok)?;
                state.serialize_field("data", data)?;
                state.end()
            }
            ToolOutput::Failure { ok, error } => {
                let mut state = serializer.serialize_struct("ToolOutput", 2)?;
                state.serialize_field("ok", ok)?;
                state.serialize_field("error", error)?;
                state.end()
            }
            ToolOutput::Canceled { message } => {
                let error = ToolError {
                    code: CANCELED_ERROR_CODE.to_string(),
                    message: message.clone(),
                    details: None,
                };
                let mut state = serializer.serialize_struct("ToolOutput", 2)?;
                state.serialize_field("ok", &false)?;
                state.serialize_field("error", &error)?;
                state.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ToolOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawToolOutput {
            ok: bool,
            #[serde(default)]
            data: Option<Value>,
            #[serde(default)]
            error: Option<ToolError>,
        }

        let raw = RawToolOutput::deserialize(deserializer)?;

        if raw.ok {
            Ok(ToolOutput::Success {
                ok: true,
                data: raw.data.unwrap_or(Value::Null),
                image: None,
            })
        } else if let Some(error) = raw.error {
            if error.code == CANCELED_ERROR_CODE {
                Ok(ToolOutput::Canceled {
                    message: error.message,
                })
            } else {
                Ok(ToolOutput::Failure { ok: false, error })
            }
        } else {
            Ok(ToolOutput::Failure {
                ok: false,
                error: ToolError {
                    code: "unknown".to_string(),
                    message: "Unknown error".to_string(),
                    details: None,
                },
            })
        }
    }
}

impl ToolOutput {
    /// Creates a successful tool output.
    pub fn success(data: Value) -> Self {
        ToolOutput::Success {
            ok: true,
            data,
            image: None,
        }
    }

    /// Creates a successful tool output with image content.
    pub fn success_with_image(data: Value, image: ImageContent) -> Self {
        ToolOutput::Success {
            ok: true,
            data,
            image: Some(image),
        }
    }

    /// Creates a failed tool output.
    pub fn failure(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<String>,
    ) -> Self {
        ToolOutput::Failure {
            ok: false,
            error: ToolError {
                code: code.into(),
                message: message.into(),
                details,
            },
        }
    }

    /// Creates a failed tool output with additional context.
    pub fn failure_with_details(
        code: impl Into<String>,
        message: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        Self::failure(code, message, Some(details.into()))
    }

    /// Creates a canceled tool output (user interrupt).
    pub fn canceled(message: impl Into<String>) -> Self {
        ToolOutput::Canceled {
            message: message.into(),
        }
    }

    /// Returns true if this output represents success.
    pub fn is_ok(&self) -> bool {
        matches!(self, ToolOutput::Success { .. })
    }

    /// Returns the data if this is a successful output.
    pub fn data(&self) -> Option<&Value> {
        match self {
            ToolOutput::Success { data, .. } => Some(data),
            ToolOutput::Failure { .. } | ToolOutput::Canceled { .. } => None,
        }
    }

    /// Returns the image content if present.
    pub fn image(&self) -> Option<&ImageContent> {
        match self {
            ToolOutput::Success { image, .. } => image.as_ref(),
            ToolOutput::Failure { .. } | ToolOutput::Canceled { .. } => None,
        }
    }

    /// Returns the error code and message if this is a failure.
    pub fn error_info(&self) -> Option<(&str, &str, Option<&str>)> {
        match self {
            ToolOutput::Failure { error, .. } => Some((
                error.code.as_str(),
                error.message.as_str(),
                error.details.as_deref(),
            )),
            _ => None,
        }
    }

    /// Converts the tool output to a JSON string for sending to the model.
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"ok":false,"error":{"code":"serialize_error","message":"Failed to serialize tool output"}}"#.to_string()
        })
    }
}

/// Error details for failed tool execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolError {
    pub code: String,
    pub message: String,
    /// Optional additional context for debugging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Image content for vision-capable models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContent {
    /// MIME type (e.g., "image/png", "image/jpeg")
    pub mime_type: String,
    /// Base64-encoded image data
    pub data: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_tool_output_success_roundtrip() {
        let output = ToolOutput::success(json!({"key": "value"}));
        let json_str = output.to_json_string();
        let parsed: ToolOutput = serde_json::from_str(&json_str).unwrap();

        assert!(parsed.is_ok());
        assert_eq!(parsed.data(), Some(&json!({"key": "value"})));
    }

    #[test]
    fn test_tool_output_failure_roundtrip() {
        let output = ToolOutput::failure(
            "test_code",
            "test message",
            Some("test details".to_string()),
        );
        let json_str = output.to_json_string();
        let parsed: ToolOutput = serde_json::from_str(&json_str).unwrap();

        assert!(!parsed.is_ok());
        let (code, message, details) = parsed.error_info().unwrap();
        assert_eq!(code, "test_code");
        assert_eq!(message, "test message");
        assert_eq!(details, Some("test details"));
    }

    #[test]
    fn test_tool_output_canceled_roundtrip() {
        let output = ToolOutput::canceled("User interrupted");
        let json_str = output.to_json_string();

        assert!(json_str.contains(r#""code":"canceled""#));
        assert!(json_str.contains(r#""message":"User interrupted""#));

        let parsed: ToolOutput = serde_json::from_str(&json_str).unwrap();
        assert!(matches!(parsed, ToolOutput::Canceled { .. }));

        if let ToolOutput::Canceled { message } = parsed {
            assert_eq!(message, "User interrupted");
        } else {
            panic!("Expected Canceled variant");
        }
    }

    #[test]
    fn test_tool_output_canceled_not_confused_with_failure() {
        let output = ToolOutput::failure("other_error", "some message", None);
        let json_str = output.to_json_string();
        let parsed: ToolOutput = serde_json::from_str(&json_str).unwrap();

        assert!(matches!(parsed, ToolOutput::Failure { .. }));
    }

    #[test]
    fn test_provider_retry_event_roundtrip() {
        let event = AgentEvent::ProviderRetry {
            kind: ErrorKind::HttpStatus,
            message: "HTTP 429: rate limited".to_string(),
            details: Some("retry-after: 2".to_string()),
            attempt: 2,
            max_retries: 3,
            delay_ms: 4000,
        };

        let json_str = serde_json::to_string(&event).unwrap();
        assert!(
            json_str.contains(r#""type":"provider_retry""#),
            "expected snake_case tag, got: {json_str}"
        );
        assert!(json_str.contains(r#""attempt":2"#));
        assert!(json_str.contains(r#""max_retries":3"#));
        assert!(json_str.contains(r#""delay_ms":4000"#));
        assert!(json_str.contains(r#""kind":"http_status""#));

        let parsed: AgentEvent = serde_json::from_str(&json_str).unwrap();
        match parsed {
            AgentEvent::ProviderRetry {
                kind,
                message,
                details,
                attempt,
                max_retries,
                delay_ms,
            } => {
                assert_eq!(kind, ErrorKind::HttpStatus);
                assert_eq!(message, "HTTP 429: rate limited");
                assert_eq!(details, Some("retry-after: 2".to_string()));
                assert_eq!(attempt, 2);
                assert_eq!(max_retries, 3);
                assert_eq!(delay_ms, 4000);
            }
            other => panic!("expected ProviderRetry, got {other:?}"),
        }
    }

    #[test]
    fn test_provider_retry_event_omits_none_details() {
        let event = AgentEvent::ProviderRetry {
            kind: ErrorKind::Timeout,
            message: "connection timed out".to_string(),
            details: None,
            attempt: 1,
            max_retries: 3,
            delay_ms: 2000,
        };

        let json_str = serde_json::to_string(&event).unwrap();
        assert!(
            !json_str.contains(r#""details""#),
            "details: None should be skipped, got: {json_str}"
        );
    }
}
