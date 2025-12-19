//! Tool system for agentic capabilities.
//!
//! This module provides a registry of tools that the agent can use,
//! along with schema definitions for the Anthropic API.

pub mod bash;
pub mod read;

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::ToolOutput;

/// Tool definition for the Anthropic API.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Result of executing a tool (for API compatibility).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    /// Creates a ToolResult from a ToolOutput.
    pub fn from_output(tool_use_id: String, output: &ToolOutput) -> Self {
        Self {
            tool_use_id,
            content: output.to_json_string(),
            is_error: !output.is_ok(),
        }
    }
}

/// Context for tool execution.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Root directory for file operations.
    pub root: PathBuf,

    /// Optional timeout for tool execution.
    pub timeout: Option<Duration>,
}

impl ToolContext {
    #[allow(dead_code)]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            timeout: None,
        }
    }

    pub fn with_timeout(root: PathBuf, timeout: Option<Duration>) -> Self {
        Self { root, timeout }
    }
}

/// Returns all available tool definitions.
pub fn all_tools() -> Vec<ToolDefinition> {
    vec![bash::definition(), read::definition()]
}

/// Executes a tool by name with the given input.
/// Returns the structured ToolOutput (envelope format).
pub async fn execute_tool(
    name: &str,
    tool_use_id: &str,
    input: &Value,
    ctx: &ToolContext,
) -> (ToolOutput, ToolResult) {
    let output = match name {
        "bash" => bash::execute(input, ctx, ctx.timeout).await,
        "read" => execute_read(input, ctx).await,
        _ => ToolOutput::failure("unknown_tool", format!("Unknown tool: {}", name)),
    };

    let result = ToolResult::from_output(tool_use_id.to_string(), &output);
    (output, result)
}

async fn execute_read(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input = input.clone();
    let ctx = ctx.clone();

    let timeout = ctx.timeout;
    let mut handle = tokio::task::spawn_blocking(move || read::execute(&input, &ctx));

    match timeout {
        Some(timeout) => match tokio::time::timeout(timeout, &mut handle).await {
            Ok(Ok(output)) => output,
            Ok(Err(_)) => ToolOutput::failure("panic", "Tool execution panicked"),
            Err(_) => {
                handle.abort();
                ToolOutput::failure(
                    "timeout",
                    format!(
                        "Tool execution timed out after {} seconds",
                        timeout.as_secs()
                    ),
                )
            }
        },
        None => match handle.await {
            Ok(output) => output,
            Err(_) => ToolOutput::failure("panic", "Tool execution panicked"),
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn test_execute_tool_times_out() {
        let temp = TempDir::new().unwrap();
        let ctx =
            ToolContext::with_timeout(temp.path().to_path_buf(), Some(Duration::from_secs(1)));
        let input = json!({"command": "sleep 2"});

        let (output, result) = execute_tool("bash", "toolu_timeout", &input, &ctx).await;
        // Timeout is still a success envelope with timed_out=true
        assert!(output.is_ok());
        assert!(result.content.contains(r#""timed_out":true"#));
    }

    #[tokio::test]
    async fn test_execute_unknown_tool() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf());
        let input = json!({});

        let (output, result) = execute_tool("unknown", "toolu_unknown", &input, &ctx).await;
        assert!(!output.is_ok());
        assert!(result.is_error);
        assert!(result.content.contains(r#""code":"unknown_tool""#));
    }
}
