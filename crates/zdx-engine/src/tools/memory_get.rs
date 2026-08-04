//! Memory get tool.
//!
//! Reads bounded native memory document snapshots by docid.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;
use crate::core::native_memory;

/// Returns the tool definition for the `memory_get` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Memory_Get".to_string(),
        description: "Read a bounded indexed native memory document snapshot by `docid` returned by Memory_Search, such as `zdxmem:v1:note:0123abcd4567ef89`. This reads the derived native index, not a canonical source file. The response includes source metadata, truncation status, byte range, and `next_start_byte` when the snapshot is truncated; pass that value back as `start_byte` to continue reading. Use this after Memory_Search when you need the indexed document behind a search hit. If you already have a thread_id and need a focused answer from canonical thread JSONL, prefer Read_Thread instead. For editing known local notes, use Read on the exact canonical file path. Old qmd `#...` docids are unsupported after the native cutover."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "docid": {
                    "type": "string",
                    "description": "Native memory docid returned by Memory_Search, such as `zdxmem:v1:note:0123abcd4567ef89`"
                },
                "start_byte": {
                    "type": "integer",
                    "description": "Byte offset to continue a truncated snapshot from; pass the previous response's `next_start_byte` (default: 0).",
                    "minimum": 0
                }
            },
            "required": ["docid"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct MemoryGetInput {
    docid: String,
    #[serde(default)]
    start_byte: Option<u64>,
}

/// Executes the memory get tool and returns indexed native memory content.
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

    let docid = input.docid.trim();
    if docid.is_empty() {
        return ToolOutput::failure("invalid_input", "docid cannot be empty", None);
    }

    let start_byte = usize::try_from(input.start_byte.unwrap_or(0)).unwrap_or(usize::MAX);
    match native_memory::get_memory_doc(docid, start_byte) {
        Ok(output) => ToolOutput::success(json!(output)),
        Err(err) => ToolOutput::failure(
            "get_failed",
            "Failed to read indexed native memory document",
            Some(err.to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn test_ctx() -> ToolContext {
        ToolContext::new(PathBuf::from("."), None)
    }

    #[test]
    fn test_definition_schema() {
        let def = definition();
        assert_eq!(def.name, "Memory_Get");
        assert!(def.description.contains("zdxmem:v1"));
        assert!(def.description.contains("bounded"));
        assert!(def.description.contains("prefer Read_Thread"));
        assert_eq!(def.input_schema["required"], json!(["docid"]));
    }

    #[test]
    fn test_rejects_empty_docid() {
        let output = execute(&json!({ "docid": "  " }), &test_ctx());

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "docid cannot be empty");
    }

    #[test]
    fn test_rejects_qmd_docid() {
        let output = execute(&json!({ "docid": "#doc123" }), &test_ctx());

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "get_failed");
        assert!(
            payload["error"]["details"]
                .as_str()
                .unwrap()
                .contains("qmd docids")
        );
    }
}
