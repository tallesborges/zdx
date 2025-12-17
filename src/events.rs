//! Engine event types for streaming and TUI.
//!
//! This module defines the contract for events emitted by the engine.
//! Events are serializable for future JSON output mode support.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    Error { message: String },

    /// Execution was interrupted (e.g., by user signal).
    Interrupted,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_assistant_delta_roundtrip() {
        let event = EngineEvent::AssistantDelta {
            text: "Hello".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EngineEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, parsed);
    }

    #[test]
    fn test_assistant_final_roundtrip() {
        let event = EngineEvent::AssistantFinal {
            text: "Complete response".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EngineEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, parsed);
    }

    #[test]
    fn test_tool_requested_roundtrip() {
        let event = EngineEvent::ToolRequested {
            id: "tool-123".to_string(),
            name: "read".to_string(),
            input: json!({"path": "test.txt"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EngineEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, parsed);
    }

    #[test]
    fn test_tool_started_roundtrip() {
        let event = EngineEvent::ToolStarted {
            id: "tool-123".to_string(),
            name: "read_file".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EngineEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, parsed);
    }

    #[test]
    fn test_tool_finished_roundtrip() {
        let output = ToolOutput::success(json!({"path": "test.txt", "content": "hello"}));
        let event = EngineEvent::ToolFinished {
            id: "tool-123".to_string(),
            result: output,
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EngineEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, parsed);
    }

    #[test]
    fn test_tool_finished_error_roundtrip() {
        let output = ToolOutput::failure("not_found", "File not found");
        let event = EngineEvent::ToolFinished {
            id: "tool-123".to_string(),
            result: output,
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EngineEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, parsed);
    }

    #[test]
    fn test_error_roundtrip() {
        let event = EngineEvent::Error {
            message: "Something went wrong".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EngineEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, parsed);
    }

    #[test]
    fn test_interrupted_roundtrip() {
        let event = EngineEvent::Interrupted;
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EngineEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, parsed);
    }

    #[test]
    fn test_serialization_format() {
        // Verify the JSON structure uses snake_case type tag
        let event = EngineEvent::AssistantDelta {
            text: "test".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"assistant_delta""#));

        let event = EngineEvent::ToolStarted {
            id: "1".to_string(),
            name: "test".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"tool_started""#));

        let event = EngineEvent::ToolRequested {
            id: "1".to_string(),
            name: "read".to_string(),
            input: json!({"path": "test.txt"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"tool_requested""#));
    }

    #[test]
    fn test_tool_output_success_format() {
        let output = ToolOutput::success(
            json!({"path": "test.txt", "content": "hello", "truncated": false, "bytes": 5}),
        );
        let json = output.to_json_string();
        assert!(json.contains(r#""ok":true"#));
        assert!(json.contains(r#""data":"#));
        assert!(output.is_ok());
    }

    #[test]
    fn test_tool_output_failure_format() {
        let output = ToolOutput::failure("not_found", "File not found: test.txt");
        let json = output.to_json_string();
        assert!(json.contains(r#""ok":false"#));
        assert!(json.contains(r#""error":"#));
        assert!(json.contains(r#""code":"not_found""#));
        assert!(json.contains(r#""message":"File not found: test.txt""#));
        assert!(!output.is_ok());
    }
}
