//! Tool system for agentic capabilities.
//!
//! This module provides a registry of tools that the agent can use,
//! along with schema definitions for the Anthropic API.

pub mod read;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

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
}

impl ToolContext {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

/// Returns all available tool definitions.
pub fn all_tools() -> Vec<ToolDefinition> {
    vec![read::definition()]
}

/// Executes a tool by name with the given input.
pub fn execute_tool(
    name: &str,
    tool_use_id: &str,
    input: &Value,
    ctx: &ToolContext,
) -> Result<ToolResult> {
    let content = match name {
        "read" => read::execute(input, ctx),
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

/// A tool use request from the model.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}
