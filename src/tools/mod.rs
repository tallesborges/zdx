//! Tool system for agentic capabilities.
//!
//! This module provides a registry of tools that the agent can use,
//! along with schema definitions for the Anthropic API.

pub mod bash;
pub mod edit;
pub mod read;
pub mod write;

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::events::ToolOutput;

/// Tool definition for the Anthropic API.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Content block within a tool result.
///
/// Anthropic API requires tool_result content to be an array of blocks
/// when including images: `[{type: "text", ...}, {type: "image", ...}]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultBlock {
    /// Text content block.
    Text { text: String },
    /// Image content block (base64 encoded).
    Image { mime_type: String, data: String },
}

/// Content of a tool result - either simple text or structured blocks.
///
/// - `Text`: Simple string content (backwards compatible, serializes as string)
/// - `Blocks`: Array of content blocks (required for images)
///
/// Uses `#[serde(untagged)]` so `Text` serializes as a plain string and
/// `Blocks` serializes as an array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    /// Simple text content (serializes as string for backwards compatibility).
    Text(String),
    /// Array of content blocks (required when including images).
    Blocks(Vec<ToolResultBlock>),
}

// Test-only helpers for ToolResultContent
#[cfg(test)]
impl ToolResultContent {
    /// Returns true if this content contains any image blocks.
    pub fn has_image(&self) -> bool {
        match self {
            ToolResultContent::Text(_) => false,
            ToolResultContent::Blocks(blocks) => blocks
                .iter()
                .any(|b| matches!(b, ToolResultBlock::Image { .. })),
        }
    }

    /// Returns the text content if this is Text variant, or the first text block's content.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ToolResultContent::Text(s) => Some(s),
            ToolResultContent::Blocks(blocks) => blocks.iter().find_map(|b| match b {
                ToolResultBlock::Text { text } => Some(text.as_str()),
                _ => None,
            }),
        }
    }
}

/// Result of executing a tool (for API compatibility).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: ToolResultContent,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    /// Creates a ToolResult from a ToolOutput.
    ///
    /// If the output contains image content, creates a Blocks content with
    /// both text (JSON envelope) and image blocks. Otherwise, creates Text content.
    pub fn from_output(tool_use_id: String, output: &ToolOutput) -> Self {
        let content = match output.image() {
            Some(image) => {
                // Create blocks with text description + image
                let text_block = ToolResultBlock::Text {
                    text: output.to_json_string(),
                };
                let image_block = ToolResultBlock::Image {
                    mime_type: image.mime_type.clone(),
                    data: image.data.clone(),
                };
                ToolResultContent::Blocks(vec![text_block, image_block])
            }
            None => ToolResultContent::Text(output.to_json_string()),
        };

        Self {
            tool_use_id,
            content,
            is_error: !output.is_ok(),
        }
    }
}

// Test-only helper for ToolResult
#[cfg(test)]
impl ToolResult {
    /// Creates a ToolResult with image content (test helper).
    pub fn with_image(
        tool_use_id: impl Into<String>,
        text: impl Into<String>,
        mime_type: impl Into<String>,
        data: impl Into<String>,
    ) -> Self {
        let blocks = vec![
            ToolResultBlock::Text { text: text.into() },
            ToolResultBlock::Image {
                mime_type: mime_type.into(),
                data: data.into(),
            },
        ];
        Self {
            tool_use_id: tool_use_id.into(),
            content: ToolResultContent::Blocks(blocks),
            is_error: false,
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
    pub fn with_timeout(root: PathBuf, timeout: Option<Duration>) -> Self {
        Self { root, timeout }
    }
}

/// Returns all available tool definitions.
pub fn all_tools() -> Vec<ToolDefinition> {
    vec![
        bash::definition(),
        edit::definition(),
        read::definition(),
        write::definition(),
    ]
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
        "edit" => execute_edit(input, ctx).await,
        "read" => execute_read(input, ctx).await,
        "write" => execute_write(input, ctx).await,
        _ => ToolOutput::failure("unknown_tool", format!("Unknown tool: {}", name)),
    };

    let result = ToolResult::from_output(tool_use_id.to_string(), &output);
    (output, result)
}

async fn execute_edit(input: &Value, ctx: &ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || edit::execute(&input, &ctx)
    })
    .await
}

async fn execute_read(input: &Value, ctx: &ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || read::execute(&input, &ctx)
    })
    .await
}

async fn execute_write(input: &Value, ctx: &ToolContext) -> ToolOutput {
    execute_blocking(ctx.timeout, {
        let input = input.clone();
        let ctx = ctx.clone();
        move || write::execute(&input, &ctx)
    })
    .await
}

/// Execute a blocking tool function with optional timeout.
async fn execute_blocking<F>(timeout: Option<Duration>, f: F) -> ToolOutput
where
    F: FnOnce() -> ToolOutput + Send + 'static,
{
    let mut handle = tokio::task::spawn_blocking(f);

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
        assert!(
            result
                .content
                .as_text()
                .unwrap()
                .contains(r#""timed_out":true"#)
        );
    }

    #[tokio::test]
    async fn test_execute_unknown_tool() {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::with_timeout(temp.path().to_path_buf(), None);
        let input = json!({});

        let (output, result) = execute_tool("unknown", "toolu_unknown", &input, &ctx).await;
        assert!(!output.is_ok());
        assert!(result.is_error);
        assert!(
            result
                .content
                .as_text()
                .unwrap()
                .contains(r#""code":"unknown_tool""#)
        );
    }
}
