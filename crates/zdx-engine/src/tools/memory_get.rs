//! Memory get tool.
//!
//! Reads indexed qmd memory documents by docid.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;
use crate::core::qmd;

/// Returns the tool definition for the `memory_get` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Memory_Get".to_string(),
        description: "Read an indexed qmd memory document by `docid` returned by Memory_Search, such as `#962e2b`. This reads qmd's indexed document content, not a canonical source file. Use this after Memory_Search when you need the full indexed document behind a search hit. If you already have a thread_id and need a focused answer from the canonical thread JSONL, prefer Read_Thread instead. For editing known local notes, use Read on the exact canonical file path."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "docid": {
                    "type": "string",
                    "description": "qmd document ID returned by Memory_Search, such as `#962e2b`"
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
}

/// Executes the memory get tool and returns indexed qmd memory content.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
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
    if !docid.starts_with('#') {
        return ToolOutput::failure("invalid_input", "docid must start with '#'", None);
    }

    let config = ctx.config.clone().unwrap_or_default();
    match qmd::get_memory_doc(&config.qmd, docid) {
        Ok(output) => ToolOutput::success(json!(output)),
        Err(err) => ToolOutput::failure(
            "get_failed",
            "Failed to read indexed memory document with qmd",
            Some(err.to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::config::Config;

    fn test_ctx() -> ToolContext {
        ToolContext::new(PathBuf::from("."), None)
    }

    #[test]
    fn test_definition_schema() {
        let def = definition();
        assert_eq!(def.name, "Memory_Get");
        assert!(def.description.contains("docid"));
        assert!(def.description.contains("qmd's indexed document content"));
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
    fn test_rejects_non_docid() {
        let output = execute(&json!({ "docid": "note:path.md" }), &test_ctx());

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "docid must start with '#'");
    }

    #[cfg(unix)]
    #[test]
    fn test_docid_reads_indexed_qmd_content() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let qmd_path = temp.path().join("qmd");
        fs::write(
            &qmd_path,
            "#!/bin/sh\nif [ \"$1\" = get ] && [ \"$2\" = '#doc123' ]; then\n  printf '# Indexed Doc\\n\\nIndexed content\\n'\n  exit 0\nfi\necho unexpected qmd args >&2\nexit 1\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&qmd_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&qmd_path, permissions).unwrap();

        let mut config = Config::default();
        config.qmd.command = qmd_path.display().to_string();
        let ctx = test_ctx().with_config(&config);

        let output = execute(&json!({ "docid": "#doc123" }), &ctx);

        assert!(output.is_ok());
        let data = output.data().unwrap();
        assert_eq!(data["docid"], "#doc123");
        assert!(
            data["content"]
                .as_str()
                .unwrap()
                .contains("Indexed content")
        );
    }
}
