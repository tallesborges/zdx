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
    pub reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<InputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<FunctionTool>>,
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
    pub strict: Option<()>,
}

impl From<&ToolDefinition> for FunctionTool {
    fn from(tool: &ToolDefinition) -> Self {
        Self {
            tool_type: "function",
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
            strict: None,
        }
    }
}
