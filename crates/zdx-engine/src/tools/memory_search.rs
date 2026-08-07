//! Memory search tool.
//!
//! Exposes native SQLite-backed memory discovery through native docids.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;
use crate::core::native_memory::{
    self, MemorySearchStrategy, MemorySource, NativeMemorySearchOptions,
};

const DEFAULT_LIMIT: usize = 10;

/// Returns the tool definition for the `memory_search` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Memory_Search".to_string(),
        description: "Search saved ZDX memory in the native SQLite index for canonical Notes, canonical Calendar files, and exported conversation threads. Returns native `zdxmem:v1:<source>:<id>` docids, source labels, file identifiers, snippets, scores, and warnings. Use `source` to target one source: `note` for the user's Notes/Second Brain, `calendar` for calendar/daily notes, or `thread` for saved conversation transcript exports. When the user says to search/find in their notes, set `source` to `note`; do not rely on `intent` for this because intent is not a filter. For finding a past conversation, prefer Thread_Search — it is the primary thread-discovery tool and supports date filters; use `source: \"thread\"` here only to search threads together with notes/calendar in one pass, or for configured semantic retrieval, and not as a second opinion on a Thread_Search that already returned results. Results are relevance-ranked best-first, with recent documents favored among comparable matches; read the best 1-3 docids with Memory_Get before rephrasing the query or switching tools, and do not treat snippets as the source of truth. Omitted `strategy` uses native lexical search. Explicit `keyword` is lexical-only for exact names, URLs, errors, commands, paths, and quoted phrases. Explicit `vector` or `hybrid` requires a complete configured embedding profile and fails clearly when embeddings are unavailable; agent searches never trigger corpus embedding. Use `intent` only with vector/hybrid when configured; it is ignored for keyword. Prefer limit 5-10. If the thread_id is already known and you need a focused answer from canonical thread JSONL, skip search and call Read_Thread directly."
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
                    "description": "Maximum number of results to return (default: 10). Prefer 5-10 for initial searches.",
                    "default": 10,
                    "minimum": 1
                },
                "strategy": {
                    "type": "string",
                    "description": "Retrieval strategy. Omit for native default lexical search; `keyword` is lexical-only. `vector` and `hybrid` require configured complete embeddings and fail clearly when unavailable.",
                    "enum": ["keyword", "vector", "hybrid"]
                },
                "source": {
                    "type": "string",
                    "description": "Optional memory source filter. Use `note` when the user asks to search/find in their notes or Second Brain; use `calendar` for calendar/daily notes; use `thread` for saved conversation transcripts. Omit to search all indexed memory sources.",
                    "enum": ["thread", "note", "calendar"]
                },
                "intent": {
                    "type": "string",
                    "description": "Optional brief disambiguating context for configured `vector`/`hybrid` searches, such as `ZDX native memory integration`. Not a filter. Ignored for keyword."
                },
                "candidate_limit": {
                    "type": "integer",
                    "description": "For configured `hybrid`, maximum candidates to fuse. Ignored by keyword/vector.",
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
    #[serde(default)]
    strategy: Option<MemorySearchStrategy>,
    #[serde(default)]
    source: Option<MemorySource>,
    #[serde(default)]
    intent: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::thread_search::deserialize_optional_usize"
    )]
    candidate_limit: Option<usize>,
}

/// Executes the memory search tool and returns native memory results.
///
/// Keyword searches stay fully local. Vector/hybrid searches load the layered
/// config for the embedding profile and embed only the query text — agent
/// calls can never trigger corpus embedding.
pub async fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
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

    let intent = input
        .intent
        .map(|intent| intent.trim().to_string())
        .filter(|intent| !intent.is_empty());
    let options = NativeMemorySearchOptions {
        query,
        limit: input.limit.unwrap_or(DEFAULT_LIMIT).max(1),
        strategy: input.strategy,
        source: input.source,
        intent,
        candidate_limit: input.candidate_limit.map(|limit| limit.max(1)),
        exclude_thread_id: ctx.current_thread_id.clone(),
    };

    let result = match options.strategy {
        Some(MemorySearchStrategy::Vector | MemorySearchStrategy::Hybrid) => {
            let memory_config = match crate::config::Config::load() {
                Ok(config) => config.memory,
                Err(err) => {
                    return ToolOutput::failure(
                        "search_failed",
                        "Failed to load config for semantic memory search",
                        Some(err.to_string()),
                    );
                }
            };
            native_memory::search_memory_with_config(&memory_config, &options).await
        }
        _ => native_memory::search_memory(&options),
    };

    match result {
        Ok(output) => ToolOutput::success(json!(output)),
        Err(err) => ToolOutput::failure(
            "search_failed",
            "Failed to search native memory",
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
        assert!(def.description.contains("native SQLite index"));
        assert!(def.description.contains("docids"));
        assert!(def.description.contains("strategy"));
        assert!(def.description.contains("source"));
        assert!(def.description.contains("hybrid"));
        assert!(def.description.contains("intent"));
        assert!(def.description.contains("Memory_Get"));
        assert!(def.description.contains("Read_Thread directly"));
    }

    #[tokio::test]
    async fn test_rejects_empty_query() {
        let ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        let output = execute(&json!({ "query": "  " }), &ctx).await;

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "query cannot be empty");
    }
}
