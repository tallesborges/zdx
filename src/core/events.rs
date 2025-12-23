//! Engine event types for streaming and TUI.
//!
//! This module defines the contract for events emitted by the engine.
//! Events are serializable for future JSON output mode support.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::providers::anthropic::{ChatMessage, ProviderErrorKind};

/// Events emitted by the engine during execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    /// Incremental text chunk from the assistant.
    AssistantDelta { text: String },

    /// Final complete response from the assistant.
    AssistantFinal { text: String },

    /// Model has decided to call a tool (before execution begins).
    ToolRequested {
        id: String,
        name: String,
        input: Value,
    },

    /// A tool invocation has started execution.
    ToolStarted { id: String, name: String },

    /// A tool invocation has completed.
    ToolFinished { id: String, result: ToolOutput },

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
    Interrupted,

    /// Turn completed successfully with final result.
    TurnComplete {
        /// Final accumulated text from the assistant.
        final_text: String,
        /// Updated message history (includes assistant responses and tool results).
        messages: Vec<ChatMessage>,
    },
}

/// Error categories for EngineEvent::Error.
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
/// - Success: `{ "ok": true, "data": { ... } }`
/// - Failure: `{ "ok": false, "error": { "code": "...", "message": "..." } }`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolOutput {
    Success { ok: bool, data: Value },
    Failure { ok: bool, error: ToolError },
}

impl ToolOutput {
    /// Creates a successful tool output.
    pub fn success(data: Value) -> Self {
        ToolOutput::Success { ok: true, data }
    }

    /// Creates a failed tool output.
    pub fn failure(code: impl Into<String>, message: impl Into<String>) -> Self {
        ToolOutput::Failure {
            ok: false,
            error: ToolError {
                code: code.into(),
                message: message.into(),
            },
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
            ToolOutput::Failure { .. } => None,
        }
    }

    /// Converts the tool output to a JSON string for sending to the model.
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| r#"{"ok":false,"error":{"code":"serialize_error","message":"Failed to serialize tool output"}}"#.to_string())
    }
}

/// Error details for failed tool execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolError {
    pub code: String,
    pub message: String,
}
