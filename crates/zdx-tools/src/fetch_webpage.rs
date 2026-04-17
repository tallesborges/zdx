//! Fetch webpage tool using Parallel Extract API.
//!
//! Allows the agent to extract clean content from URLs.
//! Requires `PARALLEL_API_KEY` environment variable.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition, ToolOutput};

const PARALLEL_EXTRACT_URL: &str = "https://api.parallel.ai/v1beta/extract";
const PARALLEL_BETA_HEADER: &str = "search-extract-2025-10-10";

fn normalize_objective(objective: Option<&str>) -> Option<&str> {
    objective.map(str::trim).filter(|value| !value.is_empty())
}

fn format_extract_errors(errors: &[ExtractError]) -> String {
    errors
        .iter()
        .map(
            |error| match (error.http_status_code, error.content.as_deref()) {
                (Some(status), Some(content)) => {
                    format!(
                        "{} [{} {}]: {}",
                        error.url, error.error_type, status, content
                    )
                }
                (Some(status), None) => format!("{} [{} {}]", error.url, error.error_type, status),
                (None, Some(content)) => {
                    format!("{} [{}]: {}", error.url, error.error_type, content)
                }
                (None, None) => format!("{} [{}]", error.url, error.error_type),
            },
        )
        .collect::<Vec<_>>()
        .join("; ")
}

/// Returns the tool definition for the `fetch_webpage` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Fetch_Webpage".to_string(),
        description: "Extract clean markdown content from a URL. Use this when you have a specific URL to read; use Web_Search when you need to discover URLs first. Provide `objective` to guide what to extract and `search_queries` to focus results — both improve extraction quality. Set `full_content: true` only when you need the complete page. Returns LLM-optimized markdown excerpts by default.".to_string(),
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
            "required": ["url"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct FetchInput {
    url: String,
    #[serde(default)]
    objective: Option<String>,
    #[serde(default, deserialize_with = "crate::string_or_vec::deserialize")]
    search_queries: Option<Vec<String>>,
    #[serde(default, deserialize_with = "super::bool_or_string::deserialize")]
    full_content: bool,
}

#[derive(Debug, Serialize)]
struct ExtractRequest {
    urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    objective: Option<String>,
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
    errors: Vec<ExtractError>,
    #[serde(default)]
    warnings: Option<Vec<ParallelWarning>>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct ParallelWarning {
    #[serde(rename = "type")]
    kind: String,
    message: String,
    #[serde(default)]
    detail: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct ExtractError {
    url: String,
    error_type: String,
    #[serde(default)]
    http_status_code: Option<u16>,
    #[serde(default)]
    content: Option<String>,
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

    let FetchInput {
        url,
        objective,
        search_queries,
        full_content,
    } = input;

    let url = url.trim();
    if url.is_empty() {
        return ToolOutput::failure("invalid_input", "url cannot be empty", None);
    }

    let objective = normalize_objective(objective.as_deref());

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
        objective: objective.map(str::to_string),
        search_queries,
        excerpts: !full_content, // excerpts if not full_content
        full_content,
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
            Some(format_extract_errors(&extract_response.errors)),
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
        assert!(!required.iter().any(|v| v == "objective"));
    }

    #[test]
    fn test_input_validation_missing_optional_objective() {
        let input = json!({"url": "https://example.com"});
        let parsed: FetchInput = serde_json::from_value(input).unwrap();
        assert!(parsed.objective.is_none());
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
        assert_eq!(parsed.objective.as_deref(), Some("test"));
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

    #[test]
    fn test_normalize_objective_treats_blank_as_missing() {
        assert_eq!(normalize_objective(Some("   ")), None);
        assert_eq!(normalize_objective(Some(" test ")), Some("test"));
    }

    #[test]
    fn test_extract_response_parses_warning_objects() {
        let response: ExtractResponse = serde_json::from_value(json!({
            "extract_id": "extract_123",
            "results": [],
            "errors": [],
            "warnings": [{
                "type": "warning",
                "message": "No objective provided; returning general excerpts.",
                "detail": null
            }]
        }))
        .unwrap();

        assert_eq!(
            response.warnings,
            Some(vec![ParallelWarning {
                kind: "warning".to_string(),
                message: "No objective provided; returning general excerpts.".to_string(),
                detail: None,
            }])
        );
    }

    #[test]
    fn test_extract_response_parses_structured_errors() {
        let response: ExtractResponse = serde_json::from_value(json!({
            "extract_id": "extract_123",
            "results": [],
            "errors": [{
                "url": "https://example.com",
                "error_type": "fetch_error",
                "http_status_code": 500,
                "content": "Error fetching content"
            }],
            "warnings": null
        }))
        .unwrap();

        assert_eq!(
            response.errors,
            vec![ExtractError {
                url: "https://example.com".to_string(),
                error_type: "fetch_error".to_string(),
                http_status_code: Some(500),
                content: Some("Error fetching content".to_string()),
            }]
        );
    }

    #[test]
    fn test_format_extract_errors_formats_structured_errors() {
        let details = format_extract_errors(&[ExtractError {
            url: "https://example.com".to_string(),
            error_type: "fetch_error".to_string(),
            http_status_code: Some(500),
            content: Some("Error fetching content".to_string()),
        }]);

        assert_eq!(
            details,
            "https://example.com [fetch_error 500]: Error fetching content"
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
