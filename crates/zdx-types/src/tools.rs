//! Tool definition and result value types.

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

impl ToolDefinition {
    /// Returns a copy with the name lowercased.
    ///
    /// Anthropic requires `PascalCase` tool names, but other providers
    /// (`OpenAI`, Gemini, `OpenRouter`) work better with lowercase.
    #[must_use]
    pub fn with_lowercase_name(&self) -> Self {
        Self {
            name: self.name.to_ascii_lowercase(),
            ..self.clone()
        }
    }
}

fn value_as_trimmed_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    let value = input.get(key)?.as_str()?.trim();
    (!value.is_empty()).then_some(value)
}

fn value_as_string_list(input: &Value, key: &str) -> Vec<String> {
    match input.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::String(item)) => {
            let item = item.trim();
            if item.is_empty() {
                Vec::new()
            } else {
                vec![item.to_string()]
            }
        }
        _ => Vec::new(),
    }
}

/// Raw, untruncated primary command/target for a tool.
///
/// Returns the underlying value verbatim (e.g. the `bash` command, the
/// `read`/`edit` file path) so a copy of "the command that ran" matches
/// exactly. Returns an empty string when the tool has no single obvious
/// command.
#[must_use]
pub fn tool_command_text(name: &str, input: &Value) -> String {
    let field = |key: &str| {
        value_as_trimmed_str(input, key)
            .unwrap_or_default()
            .to_string()
    };
    match name {
        "bash" => field("command"),
        "read" | "write" | "edit" => value_as_trimmed_str(input, "file_path")
            .or_else(|| value_as_trimmed_str(input, "path"))
            .unwrap_or_default()
            .to_string(),
        "glob" => field("pattern"),
        "grep" => match (
            value_as_trimmed_str(input, "pattern"),
            value_as_trimmed_str(input, "path"),
        ) {
            (Some(pattern), Some(path)) => format!("{pattern} {path}"),
            (Some(pattern), None) => pattern.to_string(),
            _ => String::new(),
        },
        "fetch_webpage" => field("url"),
        "read_thread" => field("thread_id"),
        "thread_search" => field("query"),
        "apply_patch" => field("patch"),
        "invoke_subagent" => field("prompt"),
        "web_search" => {
            let queries = value_as_string_list(input, "search_queries");
            if queries.is_empty() {
                field("objective")
            } else {
                queries.join("\n")
            }
        }
        _ => String::new(),
    }
}

/// The input key holding a tool's primary command/target, when it has one.
///
/// Inverse companion of [`tool_command_text`]: lets a consumer that only kept
/// the summarized command (e.g. an active-run marker) rebuild a displayable
/// input object like `{"command": "cargo build"}`.
#[must_use]
pub fn primary_input_key(name: &str) -> Option<&'static str> {
    match name {
        "bash" => Some("command"),
        "read" | "write" | "edit" => Some("file_path"),
        "glob" | "grep" => Some("pattern"),
        "fetch_webpage" => Some("url"),
        "read_thread" => Some("thread_id"),
        "thread_search" => Some("query"),
        "apply_patch" => Some("patch"),
        "invoke_subagent" => Some("prompt"),
        "web_search" => Some("objective"),
        _ => None,
    }
}

/// Content block within a tool result.
///
/// Anthropic API requires `tool_result` content to be an array of blocks
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    /// Simple text content (serializes as string for backwards compatibility).
    Text(String),
    /// Array of content blocks (required when including images).
    Blocks(Vec<ToolResultBlock>),
}

impl ToolResultContent {
    /// Returns the text content if this is Text variant, or the first text block's content.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ToolResultContent::Text(s) => Some(s),
            ToolResultContent::Blocks(blocks) => blocks.iter().find_map(|b| match b {
                ToolResultBlock::Text { text } => Some(text.as_str()),
                ToolResultBlock::Image { .. } => None,
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
    /// Creates a `ToolResult` from a `ToolOutput`.
    ///
    /// If the output contains image content, creates a Blocks content with
    /// both text (JSON envelope) and image blocks. Otherwise, creates Text content.
    pub fn from_output(tool_use_id: String, output: &ToolOutput) -> Self {
        let content = match output.image() {
            Some(image) => {
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
