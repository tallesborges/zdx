//! Thread search tool.
//!
//! Allows the agent to discover saved threads by query and date filters.

use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;
use crate::core::thread_persistence as tp;

const DEFAULT_LIMIT: usize = 20;

/// Returns the tool definition for the `thread_search` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Thread_Search".to_string(),
        description: "Search through saved ZDX conversation threads by content, keywords, file references, dates, or thread titles. Supports query/date filters and returns structured thread matches.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Optional free-text query to match thread titles and content"
                },
                "date": {
                    "type": "string",
                    "description": "Optional exact activity date filter (YYYY-MM-DD)"
                },
                "date_start": {
                    "type": "string",
                    "description": "Optional start activity date filter, inclusive (YYYY-MM-DD)"
                },
                "date_end": {
                    "type": "string",
                    "description": "Optional end activity date filter, inclusive (YYYY-MM-DD)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 20)",
                    "default": 20,
                    "minimum": 1
                }
            },
            "required": [],
            "additionalProperties": false
        }),
    }
}

fn deserialize_optional_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let value: Option<Value> = Option::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| D::Error::custom("expected positive integer"))
            .and_then(|n| {
                usize::try_from(n)
                    .map(Some)
                    .map_err(|_overflow| D::Error::custom("number too large"))
            }),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                trimmed
                    .parse::<usize>()
                    .map(Some)
                    .map_err(|_parse| D::Error::custom(format!("invalid number string: {s}")))
            }
        }
        Some(_) => Err(D::Error::custom("expected number or numeric string")),
    }
}

#[derive(Debug, Deserialize)]
struct ThreadSearchInput {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    date_start: Option<String>,
    #[serde(default)]
    date_end: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_usize")]
    limit: Option<usize>,
}

/// Executes the thread search tool and returns matching thread results.
pub fn execute(input: &Value, _ctx: &ToolContext) -> ToolOutput {
    let input: ThreadSearchInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for thread_search tool",
                Some(format!("Parse error: {e}")),
            );
        }
    };

    let date = match parse_date_filter(input.date.as_deref(), "date") {
        Ok(date) => date,
        Err(message) => return ToolOutput::failure("invalid_input", message, None),
    };

    let date_start = match parse_date_filter(input.date_start.as_deref(), "date_start") {
        Ok(date_start) => date_start,
        Err(message) => return ToolOutput::failure("invalid_input", message, None),
    };

    let date_end = match parse_date_filter(input.date_end.as_deref(), "date_end") {
        Ok(date_end) => date_end,
        Err(message) => return ToolOutput::failure("invalid_input", message, None),
    };

    if let (Some(start), Some(end)) = (date_start, date_end)
        && start > end
    {
        return ToolOutput::failure(
            "invalid_input",
            "date_start must be on or before date_end",
            None,
        );
    }

    let options = tp::ThreadSearchOptions {
        query: input.query,
        date,
        date_start,
        date_end,
        limit: input.limit.unwrap_or(DEFAULT_LIMIT).max(1),
    };

    match tp::search_threads(&options) {
        Ok(results) => ToolOutput::success(json!(results)),
        Err(e) => ToolOutput::failure(
            "search_failed",
            "Failed to search threads",
            Some(e.to_string()),
        ),
    }
}

fn parse_date_filter(raw: Option<&str>, field: &str) -> Result<Option<NaiveDate>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        .map(Some)
        .map_err(|_e| format!("invalid {field} value '{trimmed}' (expected YYYY-MM-DD)"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_definition_schema() {
        let def = definition();
        assert_eq!(def.name, "Thread_Search");
        assert!(def.description.contains("saved ZDX conversation threads"));
    }

    #[test]
    fn test_input_defaults() {
        let input = json!({});
        let parsed: ThreadSearchInput = serde_json::from_value(input).unwrap();
        assert!(parsed.query.is_none());
        assert!(parsed.date.is_none());
        assert!(parsed.date_start.is_none());
        assert!(parsed.date_end.is_none());
        assert!(parsed.limit.is_none());
    }

    #[test]
    fn test_limit_accepts_string() {
        let input = json!({"limit": "50"});
        let parsed: ThreadSearchInput = serde_json::from_value(input).unwrap();
        assert_eq!(parsed.limit, Some(50));
    }

    #[test]
    fn test_limit_empty_string_becomes_none() {
        let input = json!({"limit": "   "});
        let parsed: ThreadSearchInput = serde_json::from_value(input).unwrap();
        assert!(parsed.limit.is_none());
    }

    #[test]
    fn test_parse_date_filter_invalid() {
        let err = parse_date_filter(Some("2026-13-01"), "date").unwrap_err();
        assert!(err.contains("invalid date value"));
    }

    #[test]
    fn test_parse_date_filter_empty_is_ignored() {
        let parsed = parse_date_filter(Some("   "), "date").unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn test_execute_rejects_invalid_date() {
        let ctx = ToolContext::new(PathBuf::from("."), None);
        let output = execute(&json!({"date": "2026-13-01"}), &ctx);

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
    }

    #[test]
    fn test_execute_rejects_invalid_date_range() {
        let ctx = ToolContext::new(PathBuf::from("."), None);
        let output = execute(
            &json!({
                "date_start": "2026-12-01",
                "date_end": "2026-11-01"
            }),
            &ctx,
        );

        assert!(!output.is_ok());
        let payload = serde_json::to_value(output).unwrap();
        assert_eq!(payload["error"]["code"], "invalid_input");
        assert_eq!(
            payload["error"]["message"],
            "date_start must be on or before date_end"
        );
    }
}
