//! Transcript building from thread events.
//!
//! Pure helper function to convert thread events into UI transcript cells.

use std::collections::HashMap;

use super::HistoryCell;
use crate::core::events::ToolOutput;
use crate::core::thread_log::ThreadEvent;

/// Builds transcript cells from thread events.
///
/// Maps thread events to display cells:
/// - `Message` → `User` or `Assistant` cells
/// - `ToolUse` + `ToolResult` → `Tool` cells (paired by ID)
/// - `Thinking` → `Thinking` cells
/// - Skips `Meta` and `Interrupted` events
pub fn build_transcript_from_events(events: &[ThreadEvent]) -> Vec<HistoryCell> {
    let mut cells = Vec::new();
    // Track tool cells by ID for pairing with results
    let mut tool_cells: HashMap<String, usize> = HashMap::new();

    for event in events {
        match event {
            ThreadEvent::Meta { .. } => {
                // Skip meta events
            }
            ThreadEvent::Message { role, text, .. } => {
                let cell = match role.as_str() {
                    "user" => HistoryCell::user(text),
                    "assistant" => HistoryCell::assistant(text),
                    _ => continue,
                };
                cells.push(cell);
            }
            ThreadEvent::Thinking {
                content, signature, ..
            } => {
                // Create a finalized thinking cell
                let mut cell = HistoryCell::thinking_streaming(content);
                if let Some(sig) = signature {
                    cell.finalize_thinking(sig.clone());
                }
                cells.push(cell);
            }
            ThreadEvent::ToolUse {
                id, name, input, ..
            } => {
                // Create a running tool cell (will be updated by result)
                let cell = HistoryCell::tool_running(id, name, input.clone());
                let idx = cells.len();
                tool_cells.insert(id.clone(), idx);
                cells.push(cell);
            }
            ThreadEvent::ToolResult {
                tool_use_id,
                output,
                ..
            } => {
                // Find and update the corresponding tool cell
                if let Some(&idx) = tool_cells.get(tool_use_id)
                    && let Some(cell) = cells.get_mut(idx)
                {
                    // Deserialize the stored JSON back to ToolOutput
                    // (it was serialized via serde_json::to_value in ThreadEvent::from_agent)
                    let tool_output: ToolOutput = serde_json::from_value(output.clone())
                        .unwrap_or_else(|_| {
                            ToolOutput::failure("parse_error", "Failed to parse tool result")
                        });
                    cell.set_tool_result(tool_output);
                }
                // If no matching tool cell found, skip (incomplete pair)
            }
            ThreadEvent::Interrupted { .. } => {
                // Skip interrupted events when loading
            }
            ThreadEvent::Usage { .. } => {
                // Skip usage events when building transcript (they're for tracking only)
            }
        }
    }

    cells
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::ToolState;
    use super::*;

    #[test]
    fn test_build_transcript_from_events_empty() {
        let events: Vec<ThreadEvent> = vec![];
        let cells = build_transcript_from_events(&events);
        assert!(cells.is_empty());
    }

    #[test]
    fn test_build_transcript_from_events_messages() {
        let events = vec![
            ThreadEvent::Meta {
                schema_version: 1,
                title: None,
                root_path: None,
                ts: "2024-01-01T00:00:00Z".to_string(),
            },
            ThreadEvent::Message {
                role: "user".to_string(),
                text: "Hello".to_string(),
                ts: "2024-01-01T00:00:01Z".to_string(),
            },
            ThreadEvent::Message {
                role: "assistant".to_string(),
                text: "Hi there!".to_string(),
                ts: "2024-01-01T00:00:02Z".to_string(),
            },
        ];

        let cells = build_transcript_from_events(&events);
        assert_eq!(cells.len(), 2);

        // Verify user cell
        match &cells[0] {
            HistoryCell::User { content, .. } => {
                assert_eq!(content, "Hello");
            }
            _ => panic!("Expected User cell"),
        }

        // Verify assistant cell
        match &cells[1] {
            HistoryCell::Assistant { content, .. } => {
                assert_eq!(content, "Hi there!");
            }
            _ => panic!("Expected Assistant cell"),
        }
    }

    #[test]
    fn test_build_transcript_from_events_tool_use() {
        let events = vec![
            ThreadEvent::ToolUse {
                id: "tool-1".to_string(),
                name: "read".to_string(),
                input: json!({"path": "test.txt"}),
                ts: "2024-01-01T00:00:01Z".to_string(),
            },
            ThreadEvent::ToolResult {
                tool_use_id: "tool-1".to_string(),
                // output is a serialized ToolOutput (from ThreadEvent::from_agent)
                output: json!({"ok": true, "data": {"content": "file data"}}),
                ok: true,
                ts: "2024-01-01T00:00:02Z".to_string(),
            },
        ];

        let cells = build_transcript_from_events(&events);
        assert_eq!(cells.len(), 1);

        // Verify tool cell with result
        match &cells[0] {
            HistoryCell::Tool {
                name,
                state,
                result,
                ..
            } => {
                assert_eq!(name, "read");
                assert_eq!(*state, ToolState::Done);
                assert!(result.is_some());
            }
            _ => panic!("Expected Tool cell"),
        }
    }

    #[test]
    fn test_build_transcript_from_events_thinking() {
        let events = vec![ThreadEvent::Thinking {
            content: "Let me analyze this...".to_string(),
            signature: Some("sig123".to_string()),
            ts: "2024-01-01T00:00:01Z".to_string(),
        }];

        let cells = build_transcript_from_events(&events);
        assert_eq!(cells.len(), 1);

        // Verify thinking cell
        match &cells[0] {
            HistoryCell::Thinking {
                content,
                signature,
                is_streaming,
                ..
            } => {
                assert_eq!(content, "Let me analyze this...");
                assert_eq!(signature.as_deref(), Some("sig123"));
                assert!(!*is_streaming);
            }
            _ => panic!("Expected Thinking cell"),
        }
    }

    #[test]
    fn test_build_transcript_from_events_mixed() {
        let events = vec![
            ThreadEvent::Meta {
                schema_version: 1,
                title: None,
                root_path: None,
                ts: "2024-01-01T00:00:00Z".to_string(),
            },
            ThreadEvent::Message {
                role: "user".to_string(),
                text: "Read the file".to_string(),
                ts: "2024-01-01T00:00:01Z".to_string(),
            },
            ThreadEvent::Thinking {
                content: "Analyzing...".to_string(),
                signature: Some("sig".to_string()),
                ts: "2024-01-01T00:00:02Z".to_string(),
            },
            ThreadEvent::ToolUse {
                id: "t1".to_string(),
                name: "read".to_string(),
                input: json!({"path": "file.txt"}),
                ts: "2024-01-01T00:00:03Z".to_string(),
            },
            ThreadEvent::ToolResult {
                tool_use_id: "t1".to_string(),
                // output is a serialized ToolOutput (from ThreadEvent::from_agent)
                output: json!({"ok": true, "data": {"content": "data"}}),
                ok: true,
                ts: "2024-01-01T00:00:04Z".to_string(),
            },
            ThreadEvent::Message {
                role: "assistant".to_string(),
                text: "Done!".to_string(),
                ts: "2024-01-01T00:00:05Z".to_string(),
            },
            ThreadEvent::Interrupted {
                role: "system".to_string(),
                text: "Interrupted".to_string(),
                ts: "2024-01-01T00:00:06Z".to_string(),
            },
        ];

        let cells = build_transcript_from_events(&events);
        // Meta and Interrupted are skipped: user + thinking + tool + assistant = 4
        assert_eq!(cells.len(), 4);

        assert!(matches!(&cells[0], HistoryCell::User { .. }));
        assert!(matches!(&cells[1], HistoryCell::Thinking { .. }));
        assert!(matches!(&cells[2], HistoryCell::Tool { .. }));
        assert!(matches!(&cells[3], HistoryCell::Assistant { .. }));
    }
}
