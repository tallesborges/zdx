//! Web search tool using Parallel Search API.
//!
//! Allows the agent to search the web for information using natural language.
//! Requires `PARALLEL_API_KEY` environment variable.

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;

const PARALLEL_SEARCH_URL: &str = "https://api.parallel.ai/v1beta/search";
const PARALLEL_BETA_HEADER: &str = "search-extract-2025-10-10";

/// Returns the tool definition for the `web_search` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Web_Search".to_string(),
        description: "Search the web for current information and return ranked URLs with LLM-optimized excerpts. Use `objective` for the research goal and `search_queries` for concrete keyword searches. Do not send a `query` field. Prefer providing both `objective` and `search_queries`. Example: {\"objective\":\"When was the United Nations established? Prefer UN's websites.\",\"search_queries\":[\"Founding year UN\",\"Year of founding United Nations\"],\"max_results\":5}.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "objective": {
                    "type": "string",
                    "description": "Main natural-language research goal. Be specific about what you need, include context from your task, preferred sources (e.g., 'prefer official docs'), and freshness requirements (e.g., 'past 6 months'). Use this instead of a `query` field when describing the task in natural language."
                },
                "search_queries": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "Keyword queries to supplement the objective. Use specific terms and search operators. Providing both objective and search_queries yields best results. If you only have keywords, send them here instead of using a `query` field."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results to return (1-20, default: 10)",
                    "default": 10,
                    "minimum": 1,
                    "maximum": 20
                }
            },
            "required": [],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebSearchInput {
    #[serde(default)]
    objective: Option<String>,
    #[serde(default, deserialize_with = "super::string_or_vec::deserialize")]
    search_queries: Option<Vec<String>>,
    #[serde(
        default = "default_max_results",
        deserialize_with = "deserialize_max_results"
    )]
    max_results: u32,
}

fn default_max_results() -> u32 {
    10
}

fn deserialize_max_results<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IntOrString {
        Int(u32),
        String(String),
        Null,
    }

    match Option::<IntOrString>::deserialize(deserializer)? {
        Some(IntOrString::Int(value)) => Ok(value),
        Some(IntOrString::String(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(default_max_results())
            } else {
                trimmed.parse::<u32>().map_err(|_parse| {
                    de::Error::custom(format!("invalid number string for max_results: {raw}"))
                })
            }
        }
        Some(IntOrString::Null) | None => Ok(default_max_results()),
    }
}

fn normalize_objective(objective: Option<&str>) -> Option<&str> {
    objective.map(str::trim).filter(|value| !value.is_empty())
}

fn has_search_queries(search_queries: Option<&[String]>) -> bool {
    search_queries.is_some_and(|queries| !queries.is_empty())
}

fn parse_input(input: &Value) -> Result<WebSearchInput, ToolOutput> {
    if input.get("query").is_some() {
        return Err(ToolOutput::failure(
            "invalid_input",
            "`query` is not supported for web_search; use `objective` and/or `search_queries`",
            Some(
                "Example: {\"objective\":\"When was the United Nations established? Prefer UN's websites.\",\"search_queries\":[\"Founding year UN\"],\"max_results\":5}".to_string(),
            ),
        ));
    }

    serde_json::from_value(input.clone()).map_err(|e| {
        ToolOutput::failure(
            "invalid_input",
            "Invalid input for web_search tool",
            Some(format!("Parse error: {e}")),
        )
    })
}

fn validate_input(
    objective: Option<&str>,
    search_queries: Option<&[String]>,
    max_results: u32,
) -> Result<(), ToolOutput> {
    if objective.is_none() && !has_search_queries(search_queries) {
        return Err(ToolOutput::failure(
            "invalid_input",
            "at least one of objective or search_queries must be provided",
            None,
        ));
    }

    if objective.is_some_and(|value| value.len() > 5000) {
        return Err(ToolOutput::failure(
            "invalid_input",
            "Objective exceeds maximum length of 5000 characters",
            None,
        ));
    }

    if !(1..=20).contains(&max_results) {
        return Err(ToolOutput::failure(
            "invalid_input",
            "max_results must be between 1 and 20",
            None,
        ));
    }

    if let Some(queries) = search_queries {
        for query in queries {
            if query.len() > 200 {
                return Err(ToolOutput::failure(
                    "invalid_input",
                    format!("Search query exceeds 200 characters: \"{query}\""),
                    None,
                ));
            }
        }
    }

    Ok(())
}

fn parallel_api_key() -> Result<String, ToolOutput> {
    match std::env::var("PARALLEL_API_KEY") {
        Ok(key) if !key.is_empty() => Ok(key),
        _ => Err(ToolOutput::failure(
            "missing_api_key",
            "PARALLEL_API_KEY environment variable not set",
            Some("Set PARALLEL_API_KEY to use web search functionality".to_string()),
        )),
    }
}

async fn send_search_request(
    request: &SearchRequest,
    api_key: &str,
) -> Result<reqwest::Response, ToolOutput> {
    let client = reqwest::Client::new();
    client
        .post(PARALLEL_SEARCH_URL)
        .header("Content-Type", "application/json")
        .header("x-api-key", api_key)
        .header("parallel-beta", PARALLEL_BETA_HEADER)
        .json(request)
        .send()
        .await
        .map_err(|e| {
            ToolOutput::failure(
                "request_error",
                "Failed to send search request",
                Some(format!("HTTP error: {e}")),
            )
        })
}

async fn parse_search_response(response: reqwest::Response) -> Result<SearchResponse, ToolOutput> {
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(ToolOutput::failure(
            "http_error",
            format!("Search API returned HTTP {status}"),
            Some(body),
        ));
    }

    response.json().await.map_err(|e| {
        ToolOutput::failure(
            "parse_error",
            "Failed to parse search response",
            Some(format!("JSON error: {e}")),
        )
    })
}

#[derive(Debug, Serialize)]
struct SearchRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    objective: Option<String>,
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
    let input = match parse_input(input) {
        Ok(input) => input,
        Err(output) => return output,
    };

    let WebSearchInput {
        objective,
        search_queries,
        max_results,
    } = input;

    let objective = normalize_objective(objective.as_deref());
    if let Err(output) = validate_input(objective, search_queries.as_deref(), max_results) {
        return output;
    }
    let api_key = match parallel_api_key() {
        Ok(api_key) => api_key,
        Err(output) => return output,
    };

    // Build request
    let request = SearchRequest {
        objective: objective.map(str::to_string),
        search_queries,
        max_results,
        mode: "agentic",
    };

    let response = match send_search_request(&request, &api_key).await {
        Ok(response) => response,
        Err(output) => return output,
    };
    let search_response = match parse_search_response(response).await {
        Ok(search_response) => search_response,
        Err(output) => return output,
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
        assert!(def.description.contains("Do not send a `query` field"));

        let schema = &def.input_schema;
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.is_empty());

        let props = schema.get("properties").unwrap();
        assert!(
            props["objective"]["description"]
                .as_str()
                .unwrap()
                .contains("instead of a `query` field")
        );
        assert!(
            props["search_queries"]["description"]
                .as_str()
                .unwrap()
                .contains("instead of using a `query` field")
        );
        assert_eq!(props["search_queries"]["minItems"], json!(1));
        assert!(
            props["max_results"]["description"]
                .as_str()
                .unwrap()
                .contains("default: 10")
        );
    }

    #[test]
    fn test_input_validation_allows_missing_objective() {
        let input = json!({});
        let parsed: WebSearchInput = serde_json::from_value(input).unwrap();
        assert!(parsed.objective.is_none());
        assert!(parsed.search_queries.is_none());
    }

    #[test]
    fn test_input_defaults() {
        let input = json!({"objective": "test query"});
        let parsed: WebSearchInput = serde_json::from_value(input).unwrap();
        assert_eq!(parsed.max_results, 10);
        assert!(parsed.search_queries.is_none());
        assert_eq!(parsed.objective.as_deref(), Some("test query"));
    }

    #[test]
    fn test_input_allows_search_queries_without_objective() {
        let input = json!({
            "search_queries": ["gpt-5.3-codex", "february 2026"]
        });
        let parsed: WebSearchInput = serde_json::from_value(input).unwrap();
        assert!(parsed.objective.is_none());
        assert_eq!(
            parsed.search_queries,
            Some(vec![
                "gpt-5.3-codex".to_string(),
                "february 2026".to_string()
            ])
        );
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
    fn test_search_queries_accepts_json_array_string() {
        let input = json!({
            "objective": "test query",
            "search_queries": "[\"kotlin-lsp neovim\", \"jetbrains kotlin-lsp\"]"
        });
        let parsed: WebSearchInput = serde_json::from_value(input).unwrap();
        assert_eq!(
            parsed.search_queries,
            Some(vec![
                "kotlin-lsp neovim".to_string(),
                "jetbrains kotlin-lsp".to_string()
            ])
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

    #[test]
    fn test_max_results_accepts_string() {
        let input = json!({
            "objective": "test query",
            "max_results": "5"
        });
        let parsed: WebSearchInput = serde_json::from_value(input).unwrap();
        assert_eq!(parsed.max_results, 5);
    }

    #[test]
    fn test_max_results_blank_string_uses_default() {
        let input = json!({
            "objective": "test query",
            "max_results": "   "
        });
        let parsed: WebSearchInput = serde_json::from_value(input).unwrap();
        assert_eq!(parsed.max_results, 10);
    }

    #[test]
    fn test_normalize_objective_treats_blank_as_missing() {
        assert_eq!(normalize_objective(Some("   ")), None);
        assert_eq!(normalize_objective(Some(" test ")), Some("test"));
    }

    #[test]
    fn test_has_search_queries_detects_non_empty_queries() {
        assert!(has_search_queries(Some(&["rust tui styling".to_string()])));
        assert!(!has_search_queries(Some(&[])));
        assert!(!has_search_queries(None));
    }

    #[test]
    fn test_search_response_parses_warning_objects() {
        let response: SearchResponse = serde_json::from_value(json!({
            "search_id": "search_123",
            "results": [],
            "warnings": [{
                "type": "warning",
                "message": "No objective provided; using search queries only.",
                "detail": null
            }]
        }))
        .unwrap();

        assert_eq!(
            response.warnings,
            Some(vec![ParallelWarning {
                kind: "warning".to_string(),
                message: "No objective provided; using search queries only.".to_string(),
                detail: None,
            }])
        );
    }

    #[tokio::test]
    async fn test_execute_rejects_missing_objective_and_search_queries() {
        let ctx = ToolContext::new(PathBuf::from("."), None);
        let output = execute(&json!({"objective": "   "}), &ctx).await;

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(
            payload["error"]["message"],
            "at least one of objective or search_queries must be provided"
        );
    }

    #[tokio::test]
    async fn test_execute_rejects_legacy_query_field() {
        let ctx = ToolContext::new(PathBuf::from("."), None);
        let output = execute(
            &json!({
                "query": "codex-rs /fast option implementation GitHub",
                "max_results": 10
            }),
            &ctx,
        )
        .await;

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(
            payload["error"]["message"],
            "`query` is not supported for web_search; use `objective` and/or `search_queries`"
        );
    }
}
