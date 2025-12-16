//! Engine event types for streaming and TUI.
//!
//! This module defines the contract for events emitted by the engine.
//! Events are serializable for future JSON output mode support.

use serde::{Deserialize, Serialize};

/// Events emitted by the engine during execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    /// Incremental text chunk from the assistant.
    AssistantDelta { text: String },

    /// Final complete response from the assistant.
    AssistantFinal { text: String },

    /// A tool invocation has started.
    ToolStarted { id: String, name: String },

    /// A tool invocation has completed.
    ToolFinished { id: String, result: String },

    /// An error occurred during execution.
    Error { message: String },

    /// Execution was interrupted (e.g., by user signal).
    Interrupted,
}

#[cfg(test)]
mod tests {
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
        let event = EngineEvent::ToolFinished {
            id: "tool-123".to_string(),
            result: "file contents here".to_string(),
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
    }
}
