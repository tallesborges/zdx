//! Agent event types for streaming and TUI.
//!
//! This module defines the contract for events emitted by the agent.
//! Events are serializable for future JSON output mode support.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::providers::{ChatMessage, ProviderErrorKind, ReasoningBlock};

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

    /// Execution was interrupted (e.g., by user signal).
    Interrupted {
        /// Partial assistant text received before interruption (streaming only).
        #[serde(skip_serializing_if = "Option::is_none")]
        partial_content: Option<String>,
    },

    /// Turn completed successfully with final result.
    TurnCompleted {
        /// Final accumulated text from the assistant.
        final_text: String,
        /// Updated message history (includes assistant responses and tool results).
        messages: Vec<ChatMessage>,
    },

    /// Token usage update from the provider.
    ///
    /// Emitted at `message_start` (initial) and `message_delta` (final output tokens).
    /// The TUI accumulates these for thread-wide tracking.
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
///
/// All tool outputs must use this format:
/// - Success: `{"ok": true, "data": { ... }}`
/// - Failure: `{"ok": false, "error": { "code": "...", "message": "...", "details": "..." (optional) }}`
/// - Canceled: serializes as failure with `code: "canceled"` but deserializes back to Canceled variant
///
/// The optional `image` field is not serialized to JSON - it's handled
/// separately when building API requests for vision-capable models.
///
/// Note: The `details` field in `ToolError` is optional and will be omitted during
/// JSON serialization if empty. In the TUI, it's displayed to provide additional
/// debugging context for failed tool executions.
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
                // image is not serialized
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
                // Serialize as a failure with code="canceled"
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
            // Success case
            Ok(ToolOutput::Success {
                ok: true,
                data: raw.data.unwrap_or(Value::Null),
                image: None, // image is never serialized
            })
        } else if let Some(error) = raw.error {
            // Check if this is a canceled operation
            if error.code == CANCELED_ERROR_CODE {
                Ok(ToolOutput::Canceled {
                    message: error.message,
                })
            } else {
                Ok(ToolOutput::Failure { ok: false, error })
            }
        } else {
            // Fallback to failure with unknown error
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
    ///
    /// Convenience method equivalent to `failure(code, message, Some(details))`.
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
        // Custom Serialize impl handles all variants including Canceled
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
    /// Optional additional context for debugging (e.g., stack traces, file paths, suggested fixes).
    /// This field is optional and will be omitted if empty during serialization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Image content for vision-capable models.
///
/// Contains base64-encoded image data and MIME type.
/// Used by the `read` tool when reading image files.
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

        // Verify it serializes as a failure with code="canceled"
        assert!(json_str.contains(r#""code":"canceled""#));
        assert!(json_str.contains(r#""message":"User interrupted""#));

        // Verify it deserializes back to Canceled variant
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
        // A regular failure with a different code should NOT become Canceled
        let output = ToolOutput::failure("other_error", "some message", None);
        let json_str = output.to_json_string();
        let parsed: ToolOutput = serde_json::from_str(&json_str).unwrap();

        assert!(matches!(parsed, ToolOutput::Failure { .. }));
    }
}
