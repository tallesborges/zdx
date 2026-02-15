//! Web search tool using Parallel Search API.
//!
//! Allows the agent to search the web for information using natural language.
//! Requires `PARALLEL_API_KEY` environment variable.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;

const PARALLEL_SEARCH_URL: &str = "https://api.parallel.ai/v1beta/search";
const PARALLEL_BETA_HEADER: &str = "search-extract-2025-10-10";

/// Returns the tool definition for the `web_search` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Web_Search".to_string(),
        description: "Search the web for information using natural language. Returns LLM-optimized excerpts ranked by relevance.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "objective": {
                    "type": "string",
                    "description": "Natural language research goal (max 5000 chars). Include task context, preferred sources, and freshness needs."
                },
                "search_queries": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional keyword queries (max 200 chars each). Use with objective for best results."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results to return (1-20, default: 10)",
                    "default": 10,
                    "minimum": 1,
                    "maximum": 20
                }
            },
            "required": ["objective"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct WebSearchInput {
    objective: String,
    #[serde(default, deserialize_with = "super::string_or_vec::deserialize")]
    search_queries: Option<Vec<String>>,
    #[serde(default = "default_max_results")]
    max_results: u32,
}

fn default_max_results() -> u32 {
    10
}

#[derive(Debug, Serialize)]
struct SearchRequest {
    objective: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    search_queries: Option<Vec<String>>,
    max_results: u32,
    mode: &'static str,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    search_id: String,
    results: Vec<SearchResult>,
    #[serde(default)]
    warnings: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SearchResult {
    url: String,
    title: String,
    #[serde(default)]
    publish_date: Option<String>,
    excerpts: Vec<String>,
}

/// Executes the `web_search` tool asynchronously.
pub async fn execute(input: &Value, _ctx: &ToolContext) -> ToolOutput {
    let input: WebSearchInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for web_search tool",
                Some(format!("Parse error: {e}")),
            );
        }
    };

    let objective = input.objective.trim();
    if objective.is_empty() {
        return ToolOutput::failure("invalid_input", "objective cannot be empty", None);
    }

    // Validate objective length
    if objective.len() > 5000 {
        return ToolOutput::failure(
            "invalid_input",
            "Objective exceeds maximum length of 5000 characters",
            None,
        );
    }

    // Validate max_results
    if input.max_results < 1 || input.max_results > 20 {
        return ToolOutput::failure(
            "invalid_input",
            "max_results must be between 1 and 20",
            None,
        );
    }

    // Validate search queries if provided
    if let Some(ref queries) = input.search_queries {
        for query in queries {
            if query.len() > 200 {
                return ToolOutput::failure(
                    "invalid_input",
                    format!("Search query exceeds 200 characters: \"{query}\""),
                    None,
                );
            }
        }
    }

    // Get API key from environment
    let api_key = match std::env::var("PARALLEL_API_KEY") {
        Ok(key) if !key.is_empty() => key,
        _ => {
            return ToolOutput::failure(
                "missing_api_key",
                "PARALLEL_API_KEY environment variable not set",
                Some("Set PARALLEL_API_KEY to use web search functionality".to_string()),
            );
        }
    };

    // Build request
    let request = SearchRequest {
        objective: objective.to_string(),
        search_queries: input.search_queries,
        max_results: input.max_results,
        mode: "agentic",
    };

    // Make HTTP request
    let client = reqwest::Client::new();
    let response = match client
        .post(PARALLEL_SEARCH_URL)
        .header("Content-Type", "application/json")
        .header("x-api-key", &api_key)
        .header("parallel-beta", PARALLEL_BETA_HEADER)
        .json(&request)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return ToolOutput::failure(
                "request_error",
                "Failed to send search request",
                Some(format!("HTTP error: {e}")),
            );
        }
    };

    // Check HTTP status
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return ToolOutput::failure(
            "http_error",
            format!("Search API returned HTTP {status}"),
            Some(body),
        );
    }

    // Parse response
    let search_response: SearchResponse = match response.json().await {
        Ok(r) => r,
        Err(e) => {
            return ToolOutput::failure(
                "parse_error",
                "Failed to parse search response",
                Some(format!("JSON error: {e}")),
            );
        }
    };

    // Build successful response
    ToolOutput::success(json!({
        "search_id": search_response.search_id,
        "results": search_response.results,
        "warnings": search_response.warnings
    }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_definition_schema() {
        let def = definition();
        assert_eq!(def.name, "Web_Search");
        assert!(def.description.contains("Search the web"));

        let schema = &def.input_schema;
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|v| v == "objective"));
    }

    #[test]
    fn test_input_validation_missing_objective() {
        let input = json!({});
        let result: Result<WebSearchInput, _> = serde_json::from_value(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_input_defaults() {
        let input = json!({"objective": "test query"});
        let parsed: WebSearchInput = serde_json::from_value(input).unwrap();
        assert_eq!(parsed.max_results, 10);
        assert!(parsed.search_queries.is_none());
    }

    #[test]
    fn test_search_queries_accepts_string() {
        // LLM sometimes sends a single string instead of an array
        let input = json!({
            "objective": "test query",
            "search_queries": "gpt-5.3-codex CLI model"
        });
        let parsed: WebSearchInput = serde_json::from_value(input).unwrap();
        assert_eq!(
            parsed.search_queries,
            Some(vec!["gpt-5.3-codex CLI model".to_string()])
        );
    }

    #[test]
    fn test_search_queries_accepts_array() {
        let input = json!({
            "objective": "test query",
            "search_queries": ["gpt-5.3-codex", "february 2026"]
        });
        let parsed: WebSearchInput = serde_json::from_value(input).unwrap();
        assert_eq!(
            parsed.search_queries,
            Some(vec![
                "gpt-5.3-codex".to_string(),
                "february 2026".to_string()
            ])
        );
    }

    #[test]
    fn test_search_queries_empty_string_becomes_none() {
        let input = json!({
            "objective": "test query",
            "search_queries": ""
        });
        let parsed: WebSearchInput = serde_json::from_value(input).unwrap();
        assert!(parsed.search_queries.is_none());
    }

    #[test]
    fn test_search_queries_whitespace_string_becomes_none() {
        let input = json!({
            "objective": "test query",
            "search_queries": "   "
        });
        let parsed: WebSearchInput = serde_json::from_value(input).unwrap();
        assert!(parsed.search_queries.is_none());
    }

    #[test]
    fn test_search_queries_array_items_are_trimmed() {
        let input = json!({
            "objective": "test query",
            "search_queries": ["  alpha  ", "beta"]
        });
        let parsed: WebSearchInput = serde_json::from_value(input).unwrap();
        assert_eq!(
            parsed.search_queries,
            Some(vec!["alpha".to_string(), "beta".to_string()])
        );
    }

    #[tokio::test]
    async fn test_execute_rejects_empty_objective() {
        let ctx = ToolContext::new(PathBuf::from("."), None);
        let output = execute(&json!({"objective": "   "}), &ctx).await;

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "objective cannot be empty");
    }
}
