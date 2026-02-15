//! Fetch webpage tool using Parallel Extract API.
//!
//! Allows the agent to extract clean content from URLs.
//! Requires `PARALLEL_API_KEY` environment variable.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;

const PARALLEL_EXTRACT_URL: &str = "https://api.parallel.ai/v1beta/extract";
const PARALLEL_BETA_HEADER: &str = "search-extract-2025-10-10";

/// Returns the tool definition for the `fetch_webpage` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Fetch_Webpage".to_string(),
        description: "Extract clean markdown content from a URL. Converts any public URL into LLM-optimized markdown.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to extract content from"
                },
                "objective": {
                    "type": "string",
                    "description": "Natural language description of what you're looking for in the page"
                },
                "search_queries": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional keyword queries to focus extraction"
                },
                "full_content": {
                    "type": "boolean",
                    "description": "Return full page content instead of excerpts (default: false)",
                    "default": false
                }
            },
            "required": ["url", "objective"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct FetchInput {
    url: String,
    objective: String,
    #[serde(default, deserialize_with = "super::string_or_vec::deserialize")]
    search_queries: Option<Vec<String>>,
    #[serde(default, deserialize_with = "super::bool_or_string::deserialize")]
    full_content: bool,
}

#[derive(Debug, Serialize)]
struct ExtractRequest {
    urls: Vec<String>,
    objective: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    search_queries: Option<Vec<String>>,
    excerpts: bool,
    full_content: bool,
}

#[derive(Debug, Deserialize)]
struct ExtractResponse {
    extract_id: String,
    results: Vec<ExtractResult>,
    #[serde(default)]
    errors: Vec<String>,
    #[serde(default)]
    warnings: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ExtractResult {
    url: String,
    title: String,
    #[serde(default)]
    publish_date: Option<String>,
    #[serde(default)]
    excerpts: Option<Vec<String>>,
    #[serde(default)]
    full_content: Option<String>,
}

/// Executes the `fetch_webpage` tool asynchronously.
pub async fn execute(input: &Value, _ctx: &ToolContext) -> ToolOutput {
    let input: FetchInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for fetch_webpage tool",
                Some(format!("Parse error: {e}")),
            );
        }
    };

    let url = input.url.trim();
    if url.is_empty() {
        return ToolOutput::failure("invalid_input", "url cannot be empty", None);
    }

    let objective = input.objective.trim();
    if objective.is_empty() {
        return ToolOutput::failure("invalid_input", "objective cannot be empty", None);
    }

    // Get API key from environment
    let api_key = match std::env::var("PARALLEL_API_KEY") {
        Ok(key) if !key.is_empty() => key,
        _ => {
            return ToolOutput::failure(
                "missing_api_key",
                "PARALLEL_API_KEY environment variable not set",
                Some("Set PARALLEL_API_KEY to use fetch functionality".to_string()),
            );
        }
    };

    // Build request
    let request = ExtractRequest {
        urls: vec![url.to_string()],
        objective: objective.to_string(),
        search_queries: input.search_queries,
        excerpts: !input.full_content, // excerpts if not full_content
        full_content: input.full_content,
    };

    // Make HTTP request
    let client = reqwest::Client::new();
    let response = match client
        .post(PARALLEL_EXTRACT_URL)
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
                "Failed to send extract request",
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
            format!("Extract API returned HTTP {status}"),
            Some(body),
        );
    }

    // Parse response
    let extract_response: ExtractResponse = match response.json().await {
        Ok(r) => r,
        Err(e) => {
            return ToolOutput::failure(
                "parse_error",
                "Failed to parse extract response",
                Some(format!("JSON error: {e}")),
            );
        }
    };

    // Check for API errors
    if !extract_response.errors.is_empty() {
        return ToolOutput::failure(
            "api_error",
            format!(
                "Extract API returned {} errors",
                extract_response.errors.len()
            ),
            Some(extract_response.errors.join("; ")),
        );
    }

    // Build successful response
    ToolOutput::success(json!({
        "extract_id": extract_response.extract_id,
        "results": extract_response.results,
        "warnings": extract_response.warnings
    }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_definition_schema() {
        let def = definition();
        assert_eq!(def.name, "Fetch_Webpage");
        assert!(def.description.contains("Extract"));

        let schema = &def.input_schema;
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|v| v == "url"));
        assert!(required.iter().any(|v| v == "objective"));
    }

    #[test]
    fn test_input_validation_missing_fields() {
        let input = json!({"url": "https://example.com"});
        let result: Result<FetchInput, _> = serde_json::from_value(input);
        assert!(result.is_err()); // missing objective
    }

    #[test]
    fn test_input_defaults() {
        let input = json!({
            "url": "https://example.com",
            "objective": "test"
        });
        let parsed: FetchInput = serde_json::from_value(input).unwrap();
        assert!(!parsed.full_content);
        assert!(parsed.search_queries.is_none());
    }

    #[test]
    fn test_search_queries_accepts_string() {
        // LLM sometimes sends a single string instead of an array
        let input = json!({
            "url": "https://example.com",
            "objective": "test",
            "search_queries": "gpt-5.3-codex CLI model"
        });
        let parsed: FetchInput = serde_json::from_value(input).unwrap();
        assert_eq!(
            parsed.search_queries,
            Some(vec!["gpt-5.3-codex CLI model".to_string()])
        );
    }

    #[test]
    fn test_search_queries_accepts_array() {
        let input = json!({
            "url": "https://example.com",
            "objective": "test",
            "search_queries": ["gpt-5.3-codex", "february 2026"]
        });
        let parsed: FetchInput = serde_json::from_value(input).unwrap();
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
            "url": "https://example.com",
            "objective": "test",
            "search_queries": ""
        });
        let parsed: FetchInput = serde_json::from_value(input).unwrap();
        assert!(parsed.search_queries.is_none());
    }

    #[test]
    fn test_search_queries_whitespace_string_becomes_none() {
        let input = json!({
            "url": "https://example.com",
            "objective": "test",
            "search_queries": "   "
        });
        let parsed: FetchInput = serde_json::from_value(input).unwrap();
        assert!(parsed.search_queries.is_none());
    }

    #[test]
    fn test_search_queries_array_items_are_trimmed() {
        let input = json!({
            "url": "https://example.com",
            "objective": "test",
            "search_queries": ["  alpha  ", "beta"]
        });
        let parsed: FetchInput = serde_json::from_value(input).unwrap();
        assert_eq!(
            parsed.search_queries,
            Some(vec!["alpha".to_string(), "beta".to_string()])
        );
    }

    #[tokio::test]
    async fn test_execute_rejects_empty_url() {
        let ctx = ToolContext::new(PathBuf::from("."), None);
        let output = execute(&json!({"url": "  ", "objective": "test"}), &ctx).await;

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "url cannot be empty");
    }

    #[tokio::test]
    async fn test_execute_rejects_empty_objective() {
        let ctx = ToolContext::new(PathBuf::from("."), None);
        let output = execute(
            &json!({"url": "https://example.com", "objective": "   "}),
            &ctx,
        )
        .await;

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(payload["error"]["message"], "objective cannot be empty");
    }

    #[test]
    fn test_full_content_accepts_string_true() {
        let input = json!({
            "url": "https://example.com",
            "objective": "test",
            "full_content": "true"
        });
        let parsed: FetchInput = serde_json::from_value(input).unwrap();
        assert!(parsed.full_content);
    }

    #[test]
    fn test_full_content_accepts_string_false() {
        let input = json!({
            "url": "https://example.com",
            "objective": "test",
            "full_content": "false"
        });
        let parsed: FetchInput = serde_json::from_value(input).unwrap();
        assert!(!parsed.full_content);
    }

    #[test]
    fn test_full_content_rejects_invalid_string() {
        let input = json!({
            "url": "https://example.com",
            "objective": "test",
            "full_content": "maybe"
        });
        let result: Result<FetchInput, _> = serde_json::from_value(input);
        assert!(result.is_err());
    }
}
