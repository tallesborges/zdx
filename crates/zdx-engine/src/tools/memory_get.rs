//! Memory get tool.
//!
//! Reads canonical memory records by stable memory ref.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;
use crate::core::thread_persistence as tp;

/// Returns the tool definition for the `memory_get` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Memory_Get".to_string(),
        description: "Read a canonical ZDX memory record by memory ref, such as `thread:<thread_id>` returned by Memory_Search. For thread refs, reads `$ZDX_HOME/threads/<thread_id>.jsonl` as the source of truth and returns transcript data derived from that canonical JSONL, not exported Markdown. Use this after Memory_Search when you need canonical evidence for a returned ref. If you already have a thread_id and need a focused answer to a specific goal, prefer Read_Thread instead."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "Stable memory reference to read, such as `thread:<thread_id>`"
                }
            },
            "required": ["ref"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct MemoryGetInput {
    #[serde(rename = "ref")]
    memory_ref: String,
}

/// Executes the memory get tool and returns canonical memory data.
pub fn execute(input: &Value, _ctx: &ToolContext) -> ToolOutput {
    let input: MemoryGetInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for memory_get tool",
                Some(format!("Parse error: {e}")),
            );
        }
    };

    let memory_ref = input.memory_ref.trim();
    if memory_ref.is_empty() {
        return ToolOutput::failure("invalid_input", "ref cannot be empty", None);
    }

    let Some((source, id)) = memory_ref.split_once(':') else {
        return ToolOutput::failure("invalid_input", "ref must use '<source>:<id>' format", None);
    };

    let source = source.trim();
    let id = id.trim();
    if source.is_empty() || id.is_empty() {
        return ToolOutput::failure("invalid_input", "ref must include both source and id", None);
    }

    match source {
        "thread" => read_thread_ref(id),
        unsupported => ToolOutput::failure(
            "unsupported_source",
            format!("Unsupported memory ref source '{unsupported}'"),
            Some("Supported source types: thread".to_string()),
        ),
    }
}

fn read_thread_ref(thread_id: &str) -> ToolOutput {
    let events = match tp::load_thread_events(thread_id) {
        Ok(events) => events,
        Err(e) => {
            return ToolOutput::failure(
                "thread_not_found",
                format!("Thread '{thread_id}' not found"),
                Some(format!("Load error: {e}")),
            );
        }
    };

    if events.is_empty() {
        return ToolOutput::failure(
            "thread_not_found",
            format!("Thread '{thread_id}' not found"),
            Some("Canonical thread JSONL is missing or empty".to_string()),
        );
    }

    ToolOutput::success(json!({
        "ref": format!("thread:{thread_id}"),
        "source": "thread",
        "thread_id": thread_id,
        "title": tp::extract_title_from_events(&events),
        "root_path": tp::extract_root_path_from_events(&events),
        "event_count": events.len(),
        "transcript": tp::format_transcript(&events),
    }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::core::thread_persistence::{Thread, ThreadEvent};

    fn setup_temp_zdx_home() -> &'static TempDir {
        static ZDX_HOME: OnceLock<TempDir> = OnceLock::new();
        ZDX_HOME.get_or_init(|| {
            let temp = TempDir::new().unwrap();
            unsafe { std::env::set_var("ZDX_HOME", temp.path()) };
            temp
        })
    }

    fn test_ctx() -> ToolContext {
        ToolContext::new(PathBuf::from("."), None)
    }

    #[test]
    fn test_definition_schema() {
        let def = definition();
        assert_eq!(def.name, "Memory_Get");
        assert!(def.description.contains("thread:<thread_id>"));
        assert!(
            def.description
                .contains("$ZDX_HOME/threads/<thread_id>.jsonl")
        );
        assert!(def.description.contains("not exported Markdown"));
        assert!(def.description.contains("prefer Read_Thread"));
        assert_eq!(def.input_schema["required"], json!(["ref"]));
    }

    #[test]
    fn test_rejects_empty_ref() {
        let output = execute(&json!({ "ref": "  " }), &test_ctx());

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "ref cannot be empty");
    }

    #[test]
    fn test_rejects_malformed_ref() {
        let output = execute(&json!({ "ref": "thread-id-only" }), &test_ctx());

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(
            payload["error"]["message"],
            "ref must use '<source>:<id>' format"
        );
    }

    #[test]
    fn test_rejects_unsupported_source() {
        let output = execute(&json!({ "ref": "note:abc" }), &test_ctx());

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "unsupported_source");
        assert_eq!(
            payload["error"]["message"],
            "Unsupported memory ref source 'note'"
        );
    }

    #[test]
    fn test_missing_thread_ref_returns_clear_error() {
        let _temp = setup_temp_zdx_home();
        let output = execute(
            &json!({ "ref": "thread:missing-memory-get-thread" }),
            &test_ctx(),
        );

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "thread_not_found");
        assert_eq!(
            payload["error"]["message"],
            "Thread 'missing-memory-get-thread' not found"
        );
    }

    #[test]
    fn test_thread_ref_reads_canonical_thread_jsonl() {
        let _temp = setup_temp_zdx_home();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = format!("memory-get-{nanos}");
        let mut thread = Thread::with_id(thread_id.clone()).unwrap();
        thread
            .append(&ThreadEvent::user_message("canonical user question"))
            .unwrap();
        thread
            .append(&ThreadEvent::assistant_message(
                "canonical assistant answer",
            ))
            .unwrap();

        let output = execute(
            &json!({ "ref": format!("thread:{thread_id}") }),
            &test_ctx(),
        );

        assert!(output.is_ok());
        let data = output.data().unwrap();
        assert_eq!(data["ref"], format!("thread:{thread_id}"));
        assert_eq!(data["source"], "thread");
        assert_eq!(data["thread_id"], thread_id);
        assert_eq!(data["event_count"], 3);
        let transcript = data["transcript"].as_str().unwrap();
        assert!(transcript.contains("canonical user question"));
        assert!(transcript.contains("canonical assistant answer"));
    }
}
