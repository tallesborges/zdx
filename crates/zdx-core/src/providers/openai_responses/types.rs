//! Request/response types for OpenAI-compatible Responses API.

use serde::Serialize;

use crate::tools::ToolDefinition;

#[derive(Debug, Serialize)]
pub struct RequestBody {
    pub model: String,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "max_output_tokens")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<InputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<FunctionTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ReasoningConfig {
    pub effort: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TextConfig {
    pub verbosity: String,
}

#[derive(Debug, Serialize)]
pub struct InputItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<InputContent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Encrypted reasoning content for replay (OpenAI Responses API caching)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
    /// Summary for reasoning items (required when replaying reasoning)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Vec<SummaryItem>>,
}

/// Summary item for reasoning replay
#[derive(Debug, Serialize)]
pub struct SummaryItem {
    #[serde(rename = "type")]
    pub item_type: &'static str,
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputContent {
    InputText {
        text: String,
    },
    OutputText {
        text: String,
    },
    /// Image content (base64 data URL or HTTP URL)
    InputImage {
        /// Image URL - can be:
        /// - HTTP URL: "https://example.com/image.png"
        /// - Data URL: "data:image/png;base64,..."
        image_url: String,
        /// Detail level: "low", "high", or "auto"
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

#[derive(Debug, Serialize)]
pub struct FunctionTool {
    #[serde(rename = "type")]
    pub tool_type: &'static str,
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

impl From<&ToolDefinition> for FunctionTool {
    fn from(tool: &ToolDefinition) -> Self {
        // Use lowercase tool names for OpenAI (Anthropic requires PascalCase, others prefer lowercase)
        let tool = tool.with_lowercase_name();
        Self {
            tool_type: "function",
            name: tool.name,
            description: tool.description,
            parameters: tool.input_schema,
            // Disabled: strict mode requires all properties in `required` with nullable types,
            // but Gemini doesn't support `["type", "null"]` syntax. Cross-provider compatibility wins.
            strict: None,
        }
    }
}
