//! Memory search tool.
//!
//! Exposes qmd-backed memory discovery through qmd docids.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;
use crate::core::qmd::{self, QmdMemorySearchOptions};
use crate::core::thread_export::{self, ThreadExportOptions};

const DEFAULT_LIMIT: usize = 20;

/// Returns the tool definition for the `memory_search` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Memory_Search".to_string(),
        description: "Search saved ZDX memory in qmd-backed collections for exported conversation threads, canonical Notes, and canonical Calendar files. Returns qmd `docid` handles such as `#962e2b`, qmd file identifiers, snippets, and scores. Use Memory_Get with a returned docid to read the indexed qmd document; do not treat snippets as the source of truth. If the thread_id is already known and you need a focused answer from canonical thread JSONL, skip search and call Read_Thread directly."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query for saved memory"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 20)",
                    "default": 20,
                    "minimum": 1
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct MemorySearchInput {
    query: String,
    #[serde(
        default,
        deserialize_with = "super::thread_search::deserialize_optional_usize"
    )]
    limit: Option<usize>,
}

/// Executes the memory search tool and returns qmd-backed memory results.
pub fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: MemorySearchInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for memory_search tool",
                Some(format!("Parse error: {e}")),
            );
        }
    };

    let query = input.query.trim().to_string();
    if query.is_empty() {
        return ToolOutput::failure("invalid_input", "query cannot be empty", None);
    }

    let mut warnings = Vec::new();
    match thread_export::export_threads_incremental(ThreadExportOptions::default()) {
        Ok(summary) => {
            if summary.exported > 0 || summary.removed > 0 || summary.failed > 0 {
                warnings.push(format!(
                    "thread exports changed before search (exported={}, removed={}, failed={}); run `zdx memory index` to refresh qmd if results look stale",
                    summary.exported, summary.removed, summary.failed
                ));
            }
        }
        Err(err) => {
            warnings.push(format!(
                "could not refresh thread exports before search: {err}"
            ));
        }
    }

    let config = ctx.config.clone().unwrap_or_default();
    let options = QmdMemorySearchOptions {
        query,
        limit: input.limit.unwrap_or(DEFAULT_LIMIT).max(1),
        exclude_thread_id: ctx.current_thread_id.clone(),
    };

    match qmd::search_memory_collections(&config.qmd, &config.memory, &options) {
        Ok(mut output) => {
            warnings.append(&mut output.warnings);
            output.warnings = warnings;
            ToolOutput::success(json!(output))
        }
        Err(err) => ToolOutput::failure(
            "search_failed",
            "Failed to search memory with qmd",
            Some(err.to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_definition_schema() {
        let def = definition();
        assert_eq!(def.name, "Memory_Search");
        assert!(def.description.contains("qmd-backed collections"));
        assert!(def.description.contains("docid"));
        assert!(def.description.contains("Memory_Get"));
        assert!(def.description.contains("Read_Thread directly"));
    }

    #[test]
    fn test_rejects_empty_query() {
        let ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        let output = execute(&json!({ "query": "  " }), &ctx);

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "query cannot be empty");
    }
}
