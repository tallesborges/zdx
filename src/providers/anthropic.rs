//! Anthropic Claude API client.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::{ToolDefinition, ToolResult, ToolUse};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Configuration for the Anthropic client.
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
}

impl AnthropicConfig {
    /// Creates a new config from environment and provided settings.
    ///
    /// Environment variables:
    /// - `ANTHROPIC_API_KEY`: Required API key
    /// - `ANTHROPIC_BASE_URL`: Optional base URL override
    pub fn from_env(model: String, max_tokens: u32) -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .context("ANTHROPIC_API_KEY environment variable is not set")?;

        let base_url =
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
        })
    }
}

/// Anthropic API client.
pub struct AnthropicClient {
    config: AnthropicConfig,
    http: reqwest::Client,
}

impl AnthropicClient {
    /// Creates a new Anthropic client with the given configuration.
    pub fn new(config: AnthropicConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Sends a message and returns the assistant's text response.
    #[allow(dead_code)] // Useful API for simpler use cases
    pub async fn send_message(&self, prompt: &str) -> Result<String> {
        let response = self
            .send_messages(&[ChatMessage::user(prompt)], &[])
            .await?;
        response.text().context("No text content in response")
    }

    /// Sends a conversation and returns the full response with content blocks.
    pub async fn send_messages(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<AssistantResponse> {
        let api_messages: Vec<ApiMessage> = messages.iter().map(ApiMessage::from).collect();

        let request = if tools.is_empty() {
            MessagesRequest {
                model: &self.config.model,
                max_tokens: self.config.max_tokens,
                messages: api_messages,
                tools: None,
            }
        } else {
            let tool_defs: Vec<ApiToolDef> = tools.iter().map(ApiToolDef::from).collect();
            MessagesRequest {
                model: &self.config.model,
                max_tokens: self.config.max_tokens,
                messages: api_messages,
                tools: Some(tool_defs),
            }
        };

        let url = format!("{}/v1/messages", self.config.base_url);

        let response = self
            .http
            .post(&url)
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Anthropic API")?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            bail!(
                "Anthropic API request failed with status {}: {}",
                status,
                error_body
            );
        }

        let raw: MessagesResponse = response
            .json()
            .await
            .context("Failed to parse Anthropic API response")?;

        Ok(AssistantResponse::from(raw))
    }
}

/// A content block in the response.
#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text(String),
    ToolUse(ToolUse),
}

/// The assistant's response, parsed into content blocks.
#[derive(Debug, Clone)]
pub struct AssistantResponse {
    pub content: Vec<ContentBlock>,
    #[allow(dead_code)] // Useful for future features
    pub stop_reason: String,
}

impl AssistantResponse {
    /// Extracts all text blocks concatenated.
    pub fn text(&self) -> Option<String> {
        let texts: Vec<&str> = self
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();

        if texts.is_empty() {
            None
        } else {
            Some(texts.join("\n"))
        }
    }

    /// Returns all tool use requests.
    pub fn tool_uses(&self) -> Vec<&ToolUse> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse(tu) => Some(tu),
                _ => None,
            })
            .collect()
    }

    /// Returns true if the model wants to use tools.
    pub fn has_tool_use(&self) -> bool {
        self.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse(_)))
    }
}

impl From<MessagesResponse> for AssistantResponse {
    fn from(raw: MessagesResponse) -> Self {
        let content = raw
            .content
            .into_iter()
            .filter_map(|block| match block.block_type.as_str() {
                "text" => Some(ContentBlock::Text(block.text.unwrap_or_default())),
                "tool_use" => {
                    let tu = ToolUse {
                        id: block.id.unwrap_or_default(),
                        name: block.name.unwrap_or_default(),
                        input: block.input.unwrap_or(Value::Null),
                    };
                    Some(ContentBlock::ToolUse(tu))
                }
                _ => None,
            })
            .collect();

        Self {
            content,
            stop_reason: raw.stop_reason.unwrap_or_default(),
        }
    }
}

// === API Request Types ===

#[derive(Debug, Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ApiToolDef<'a>>>,
}

#[derive(Debug, Serialize)]
struct ApiToolDef<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a Value,
}

impl<'a> From<&'a ToolDefinition> for ApiToolDef<'a> {
    fn from(def: &'a ToolDefinition) -> Self {
        Self {
            name: &def.name,
            description: &def.description,
            input_schema: &def.input_schema,
        }
    }
}

#[derive(Debug, Serialize)]
struct ApiMessage {
    role: String,
    content: ApiMessageContent,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ApiMessageContent {
    Text(String),
    Blocks(Vec<ApiContentBlock>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ApiContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

impl From<&ChatMessage> for ApiMessage {
    fn from(msg: &ChatMessage) -> Self {
        match &msg.content {
            MessageContent::Text(text) => ApiMessage {
                role: msg.role.clone(),
                content: ApiMessageContent::Text(text.clone()),
            },
            MessageContent::Blocks(blocks) => {
                let api_blocks: Vec<ApiContentBlock> = blocks
                    .iter()
                    .map(|b| match b {
                        ChatContentBlock::Text(text) => {
                            ApiContentBlock::Text { text: text.clone() }
                        }
                        ChatContentBlock::ToolUse { id, name, input } => ApiContentBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        },
                        ChatContentBlock::ToolResult(result) => ApiContentBlock::ToolResult {
                            tool_use_id: result.tool_use_id.clone(),
                            content: result.content.clone(),
                            is_error: result.is_error,
                        },
                    })
                    .collect();
                ApiMessage {
                    role: msg.role.clone(),
                    content: ApiMessageContent::Blocks(api_blocks),
                }
            }
        }
    }
}

// === API Response Types ===

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    content: Vec<RawContentBlock>,
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

// === Public Chat Types ===

/// Content block in a chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChatContentBlock {
    #[serde(rename = "text")]
    Text(String),
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult(ToolResult),
}

/// Message content - either simple text or structured blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ChatContentBlock>),
}

/// A chat message with owned data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: MessageContent,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: MessageContent::Text(content.into()),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: MessageContent::Text(content.into()),
        }
    }

    /// Creates an assistant message with content blocks (for tool use).
    pub fn assistant_blocks(blocks: Vec<ChatContentBlock>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: MessageContent::Blocks(blocks),
        }
    }

    /// Creates a user message with tool results.
    pub fn tool_results(results: Vec<ToolResult>) -> Self {
        let blocks: Vec<ChatContentBlock> = results
            .into_iter()
            .map(ChatContentBlock::ToolResult)
            .collect();
        Self {
            role: "user".to_string(),
            content: MessageContent::Blocks(blocks),
        }
    }

    /// Returns the text content if this is a simple text message.
    #[allow(dead_code)] // Useful API for simple text extraction
    pub fn text(&self) -> Option<&str> {
        match &self.content {
            MessageContent::Text(t) => Some(t),
            MessageContent::Blocks(blocks) => {
                // Return first text block if any
                blocks.iter().find_map(|b| match b {
                    ChatContentBlock::Text(t) => Some(t.as_str()),
                    _ => None,
                })
            }
        }
    }
}
