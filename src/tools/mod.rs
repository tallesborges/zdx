//! Tool system for agentic capabilities.
//!
//! This module provides a registry of tools that the agent can use,
//! along with schema definitions for the Anthropic API.

pub mod bash;
pub mod read;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

/// Tool definition for the Anthropic API.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Result of executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
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
pub async fn execute_tool(
    name: &str,
    tool_use_id: &str,
    input: &Value,
    ctx: &ToolContext,
) -> Result<ToolResult> {
    let content = match name {
        "bash" => bash::execute(input, ctx, ctx.timeout).await,
        "read" => execute_read(input, ctx).await,
        _ => Err(anyhow::anyhow!("Unknown tool: {}", name)),
    };

    match content {
        Ok(text) => Ok(ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: text,
            is_error: false,
        }),
        Err(e) => Ok(ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: format!("Error: {}", e),
            is_error: true,
        }),
    }
}

async fn execute_read(input: &Value, ctx: &ToolContext) -> Result<String> {
    let input = input.clone();
    let ctx = ctx.clone();

    let timeout = ctx.timeout;
    let mut handle = tokio::task::spawn_blocking(move || read::execute(&input, &ctx));

    match timeout {
        Some(timeout) => match tokio::time::timeout(timeout, &mut handle).await {
            Ok(joined) => joined.context("tool execution panicked")?,
            Err(_) => {
                handle.abort();
                anyhow::bail!(
                    "Tool execution timed out after {} seconds",
                    timeout.as_secs()
                );
            }
        },
        None => handle.await.context("tool execution panicked")?,
    }
}

/// A tool use request from the model.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_execute_tool_times_out() {
        let temp = TempDir::new().unwrap();
        let ctx =
            ToolContext::with_timeout(temp.path().to_path_buf(), Some(Duration::from_secs(1)));
        let input = json!({"command": "sleep 2"});

        let result = execute_tool("bash", "toolu_timeout", &input, &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("timed out"));
    }
}
