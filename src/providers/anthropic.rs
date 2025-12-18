//! Anthropic Claude API client.

use std::fmt;
use std::pin::Pin;

use anyhow::{Context, Result, bail};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::{ToolDefinition, ToolResult, ToolUse};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

// === Structured Provider Errors ===

/// Categories of provider errors for consistent error handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    /// HTTP status error (4xx, 5xx)
    HttpStatus,
    /// Connection timeout or request timeout
    Timeout,
    /// Failed to parse response (JSON parse error, invalid SSE, etc.)
    Parse,
    /// API-level error returned by the provider (e.g., overloaded, rate_limit)
    ApiError,
}

impl fmt::Display for ProviderErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderErrorKind::HttpStatus => write!(f, "http_status"),
            ProviderErrorKind::Timeout => write!(f, "timeout"),
            ProviderErrorKind::Parse => write!(f, "parse"),
            ProviderErrorKind::ApiError => write!(f, "api_error"),
        }
    }
}

/// Structured error from the provider with kind and details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderError {
    /// Error category
    pub kind: ProviderErrorKind,
    /// One-line summary suitable for display
    pub message: String,
    /// Optional additional details (e.g., raw error body)
    pub details: Option<String>,
}

impl ProviderError {
    /// Creates a new provider error.
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
        }
    }

    /// Creates a provider error with details.
    pub fn with_details(
        kind: ProviderErrorKind,
        message: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            details: Some(details.into()),
        }
    }

    /// Creates an HTTP status error.
    pub fn http_status(status: u16, body: &str) -> Self {
        let message = format!("HTTP {}", status);
        let details = if body.is_empty() {
            None
        } else {
            // Try to extract a cleaner error message from JSON
            if let Ok(json) = serde_json::from_str::<Value>(body) {
                if let Some(error_obj) = json.get("error") {
                    if let Some(msg) = error_obj.get("message").and_then(|v| v.as_str()) {
                        return Self {
                            kind: ProviderErrorKind::HttpStatus,
                            message: format!("HTTP {}: {}", status, msg),
                            details: Some(body.to_string()),
                        };
                    }
                }
            }
            Some(body.to_string())
        };
        Self {
            kind: ProviderErrorKind::HttpStatus,
            message,
            details,
        }
    }

    /// Creates a timeout error.
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Timeout, message)
    }

    /// Creates a parse error.
    pub fn parse(message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Parse, message)
    }

    /// Creates an API error (from mid-stream error event).
    pub fn api_error(error_type: &str, message: &str) -> Self {
        Self {
            kind: ProviderErrorKind::ApiError,
            message: format!("{}: {}", error_type, message),
            details: None,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ProviderError {}

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
    ///
    /// Base URL resolution order:
    /// 1. `ANTHROPIC_BASE_URL` env var (if set and non-empty)
    /// 2. `config_base_url` parameter (if Some and non-empty)
    /// 3. Default: `https://api.anthropic.com`
    pub fn from_env(model: String, max_tokens: u32, config_base_url: Option<&str>) -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .context("ANTHROPIC_API_KEY environment variable is not set")?;

        // Resolution order: env > config > default
        let base_url = Self::resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
        })
    }

    /// Resolves the base URL with precedence: env > config > default.
    /// Validates that the URL is well-formed.
    fn resolve_base_url(config_base_url: Option<&str>) -> Result<String> {
        // Try env var first
        if let Ok(env_url) = std::env::var("ANTHROPIC_BASE_URL") {
            let trimmed = env_url.trim();
            if !trimmed.is_empty() {
                Self::validate_url(trimmed)?;
                return Ok(trimmed.to_string());
            }
        }

        // Try config value
        if let Some(config_url) = config_base_url {
            let trimmed = config_url.trim();
            if !trimmed.is_empty() {
                Self::validate_url(trimmed)?;
                return Ok(trimmed.to_string());
            }
        }

        // Default
        Ok(DEFAULT_BASE_URL.to_string())
    }

    /// Validates that a URL is well-formed.
    fn validate_url(url: &str) -> Result<()> {
        url::Url::parse(url).with_context(|| format!("Invalid Anthropic base URL: {}", url))?;
        Ok(())
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
            .send_messages(&[ChatMessage::user(prompt)], &[], None)
            .await?;
        response.text().context("No text content in response")
    }

    /// Sends a conversation and returns the full response with content blocks.
    pub async fn send_messages(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<AssistantResponse> {
        let api_messages: Vec<ApiMessage> = messages.iter().map(ApiMessage::from).collect();

        let tool_defs = if tools.is_empty() {
            None
        } else {
            Some(tools.iter().map(ApiToolDef::from).collect::<Vec<_>>())
        };

        let request = MessagesRequest {
            model: &self.config.model,
            max_tokens: self.config.max_tokens,
            messages: api_messages,
            tools: tool_defs,
            system,
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
            .map_err(|e| Self::classify_reqwest_error(e))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::http_status(status.as_u16(), &error_body).into());
        }

        let raw: MessagesResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::parse(format!("Failed to parse response: {}", e)))?;

        Ok(AssistantResponse::from(raw))
    }

    /// Sends a conversation and returns an async stream of events.
    ///
    /// This enables chunk-by-chunk token streaming from the API.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let api_messages: Vec<ApiMessage> = messages.iter().map(ApiMessage::from).collect();

        let tool_defs = if tools.is_empty() {
            None
        } else {
            Some(tools.iter().map(ApiToolDef::from).collect::<Vec<_>>())
        };

        let request = StreamingMessagesRequest {
            model: &self.config.model,
            max_tokens: self.config.max_tokens,
            messages: api_messages,
            tools: tool_defs,
            system,
            stream: true,
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
            .map_err(|e| Self::classify_reqwest_error(e))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::http_status(status.as_u16(), &error_body).into());
        }

        let byte_stream = response.bytes_stream();
        let event_stream = SseParser::new(byte_stream);
        Ok(Box::pin(event_stream))
    }

    /// Classifies a reqwest error into a ProviderError.
    fn classify_reqwest_error(e: reqwest::Error) -> ProviderError {
        if e.is_timeout() {
            ProviderError::timeout(format!("Request timed out: {}", e))
        } else if e.is_connect() {
            ProviderError::timeout(format!("Connection failed: {}", e))
        } else if e.is_request() {
            ProviderError::new(
                ProviderErrorKind::HttpStatus,
                format!("Request error: {}", e),
            )
        } else {
            ProviderError::new(
                ProviderErrorKind::HttpStatus,
                format!("Network error: {}", e),
            )
        }
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

// === Streaming Types ===

/// Events emitted during streaming.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Message started, contains model info
    MessageStart { model: String },
    /// A content block has started (text or tool_use)
    ContentBlockStart {
        index: usize,
        block_type: String,
        /// For tool_use blocks: the tool use ID
        id: Option<String>,
        /// For tool_use blocks: the tool name
        name: Option<String>,
    },
    /// Text delta within a content block
    TextDelta { index: usize, text: String },
    /// Partial JSON delta for tool input
    InputJsonDelta { index: usize, partial_json: String },
    /// A content block has ended
    ContentBlockStop { index: usize },
    /// Message delta (e.g., stop_reason update)
    MessageDelta { stop_reason: Option<String> },
    /// Message completed
    MessageStop,
    /// Ping event (keepalive)
    Ping,
    /// Error event from API
    Error { error_type: String, message: String },
}

/// SSE parser that converts a byte stream into StreamEvents.
pub struct SseParser<S> {
    inner: S,
    buffer: Vec<u8>,
}

impl<S> SseParser<S> {
    pub fn new(stream: S) -> Self {
        Self {
            inner: stream,
            buffer: Vec::new(),
        }
    }
}

impl<S, E> Stream for SseParser<S>
where
    S: Stream<Item = std::result::Result<bytes::Bytes, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
{
    type Item = Result<StreamEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;

        loop {
            // Check if we have a complete event in the buffer
            if let Some(event) = self.try_parse_event() {
                return Poll::Ready(Some(event));
            }

            // Try to get more data from the underlying stream
            let inner = Pin::new(&mut self.inner);
            match inner.poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    self.buffer.extend_from_slice(&bytes);
                    // Continue looping to parse
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(anyhow::anyhow!("Stream error: {}", e))));
                }
                Poll::Ready(None) => {
                    // Stream ended - check for any remaining buffered event
                    let is_empty = self.buffer.iter().all(|b| b.is_ascii_whitespace());
                    if is_empty {
                        return Poll::Ready(None);
                    }
                    // Try to parse remaining buffer
                    if let Some(event) = self.try_parse_event() {
                        return Poll::Ready(Some(event));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<S> SseParser<S> {
    /// Tries to parse a complete SSE event from the buffer.
    /// Returns None if no complete event is available yet.
    fn try_parse_event(&mut self) -> Option<Result<StreamEvent>> {
        // SSE events are separated by double newlines
        // Find \n\n in the byte buffer
        let event_end = self.buffer.windows(2).position(|w| w == b"\n\n")?;

        // Extract the event bytes and remove from buffer
        let event_bytes: Vec<u8> = self.buffer.drain(..event_end).collect();
        self.buffer.drain(..2); // remove the \n\n delimiter

        // Decode UTF-8 only after we have the complete event
        let event_text = match std::str::from_utf8(&event_bytes) {
            Ok(s) => s,
            Err(e) => return Some(Err(anyhow::anyhow!("Invalid UTF-8 in SSE event: {}", e))),
        };

        Some(parse_sse_event(event_text))
    }
}

/// Parses a single SSE event block into a StreamEvent.
pub fn parse_sse_event(event_text: &str) -> Result<StreamEvent> {
    let mut event_type = None;
    let mut data = None;

    for line in event_text.lines() {
        if let Some(value) = line.strip_prefix("event: ") {
            event_type = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data: ") {
            data = Some(value);
        }
    }

    let event_type = event_type.unwrap_or("message");

    match event_type {
        "ping" => Ok(StreamEvent::Ping),
        "message_start" => {
            let data = data.context("Missing data for message_start event")?;
            let parsed: SseMessageStart =
                serde_json::from_str(data).context("Failed to parse message_start")?;
            Ok(StreamEvent::MessageStart {
                model: parsed.message.model,
            })
        }
        "content_block_start" => {
            let data = data.context("Missing data for content_block_start event")?;
            let parsed: SseContentBlockStart =
                serde_json::from_str(data).context("Failed to parse content_block_start")?;
            Ok(StreamEvent::ContentBlockStart {
                index: parsed.index,
                block_type: parsed.content_block.block_type,
                id: parsed.content_block.id,
                name: parsed.content_block.name,
            })
        }
        "content_block_delta" => {
            let data = data.context("Missing data for content_block_delta event")?;
            let parsed: SseContentBlockDelta =
                serde_json::from_str(data).context("Failed to parse content_block_delta")?;
            match parsed.delta.delta_type.as_str() {
                "text_delta" => Ok(StreamEvent::TextDelta {
                    index: parsed.index,
                    text: parsed.delta.text.unwrap_or_default(),
                }),
                "input_json_delta" => Ok(StreamEvent::InputJsonDelta {
                    index: parsed.index,
                    partial_json: parsed.delta.partial_json.unwrap_or_default(),
                }),
                other => bail!("Unknown delta type: {}", other),
            }
        }
        "content_block_stop" => {
            let data = data.context("Missing data for content_block_stop event")?;
            let parsed: SseContentBlockStop =
                serde_json::from_str(data).context("Failed to parse content_block_stop")?;
            Ok(StreamEvent::ContentBlockStop {
                index: parsed.index,
            })
        }
        "message_delta" => {
            let data = data.context("Missing data for message_delta event")?;
            let parsed: SseMessageDelta =
                serde_json::from_str(data).context("Failed to parse message_delta")?;
            Ok(StreamEvent::MessageDelta {
                stop_reason: parsed.delta.stop_reason.clone(),
            })
        }
        "message_stop" => Ok(StreamEvent::MessageStop),
        "error" => {
            let data = data.context("Missing data for error event")?;
            let parsed: SseError = serde_json::from_str(data).context("Failed to parse error")?;
            Ok(StreamEvent::Error {
                error_type: parsed.error.error_type,
                message: parsed.error.message,
            })
        }
        other => bail!("Unknown SSE event type: {}", other),
    }
}

// === SSE Response Structures ===

#[derive(Debug, Deserialize)]
struct SseMessageStart {
    message: SseMessageInfo,
}

#[derive(Debug, Deserialize)]
struct SseMessageInfo {
    model: String,
}

#[derive(Debug, Deserialize)]
struct SseContentBlockStart {
    index: usize,
    content_block: SseContentBlock,
}

#[derive(Debug, Deserialize)]
struct SseContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SseContentBlockDelta {
    index: usize,
    delta: SseDelta,
}

#[derive(Debug, Deserialize)]
struct SseDelta {
    #[serde(rename = "type")]
    delta_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SseContentBlockStop {
    index: usize,
}

#[derive(Debug, Deserialize)]
struct SseMessageDelta {
    delta: SseMessageDeltaInner,
}

#[derive(Debug, Deserialize)]
struct SseMessageDeltaInner {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SseError {
    error: SseErrorInfo,
}

#[derive(Debug, Deserialize)]
struct SseErrorInfo {
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}

// === API Request Types ===

#[derive(Debug, Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ApiToolDef<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct StreamingMessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ApiToolDef<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    stream: bool,
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

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;

    use super::*;

    /// SSE fixture simulating a typical Anthropic streaming response
    const SSE_TEXT_RESPONSE: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: ping
data: {"type":"ping"}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"!"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}

event: message_stop
data: {"type":"message_stop"}

"#;

    /// SSE fixture simulating a tool use streaming response
    const SSE_TOOL_USE_RESPONSE: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_456","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":20,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_abc123","name":"get_weather"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"location\": \"San"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":" Francisco\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":25}}

event: message_stop
data: {"type":"message_stop"}

"#;

    /// SSE fixture simulating an error mid-stream
    const SSE_ERROR_RESPONSE: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_789","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}

event: error
data: {"type":"error","error":{"type":"overloaded_error","message":"API is temporarily overloaded"}}

"#;

    /// Helper to create a mock byte stream from a string
    fn mock_byte_stream(
        data: &str,
    ) -> impl Stream<Item = std::result::Result<bytes::Bytes, std::io::Error>> {
        let chunks: Vec<_> = data
            .as_bytes()
            .chunks(50) // Simulate chunked delivery
            .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
            .collect();
        futures_util::stream::iter(chunks)
    }

    #[tokio::test]
    async fn test_sse_parser_text_response() {
        let stream = mock_byte_stream(SSE_TEXT_RESPONSE);
        let mut parser = SseParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.expect("Expected valid event"));
        }

        // Verify we got all expected events
        assert_eq!(events.len(), 9);

        // Check specific events
        assert!(
            matches!(&events[0], StreamEvent::MessageStart { model } if model == "claude-sonnet-4-20250514")
        );
        assert!(matches!(
            &events[1],
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type,
                ..
            } if block_type == "text"
        ));
        assert_eq!(events[2], StreamEvent::Ping);
        assert_eq!(
            events[3],
            StreamEvent::TextDelta {
                index: 0,
                text: "Hello".to_string()
            }
        );
        assert_eq!(
            events[4],
            StreamEvent::TextDelta {
                index: 0,
                text: " world".to_string()
            }
        );
        assert_eq!(
            events[5],
            StreamEvent::TextDelta {
                index: 0,
                text: "!".to_string()
            }
        );
        assert_eq!(events[6], StreamEvent::ContentBlockStop { index: 0 });
        assert!(matches!(
            &events[7],
            StreamEvent::MessageDelta {
                stop_reason: Some(reason)
            } if reason == "end_turn"
        ));
        assert_eq!(events[8], StreamEvent::MessageStop);
    }

    #[tokio::test]
    async fn test_sse_parser_tool_use_response() {
        let stream = mock_byte_stream(SSE_TOOL_USE_RESPONSE);
        let mut parser = SseParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.expect("Expected valid event"));
        }

        // Verify we got all expected events
        assert_eq!(events.len(), 8);

        // Check tool_use specific events
        assert!(matches!(
            &events[1],
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type,
                id: Some(id),
                name: Some(name),
            } if block_type == "tool_use" && id == "toolu_abc123" && name == "get_weather"
        ));

        // Check input_json_delta events
        assert_eq!(
            events[2],
            StreamEvent::InputJsonDelta {
                index: 0,
                partial_json: "{\"".to_string()
            }
        );
        assert_eq!(
            events[3],
            StreamEvent::InputJsonDelta {
                index: 0,
                partial_json: "location\": \"San".to_string()
            }
        );
        assert_eq!(
            events[4],
            StreamEvent::InputJsonDelta {
                index: 0,
                partial_json: " Francisco\"}".to_string()
            }
        );

        // Check stop_reason is tool_use
        assert!(matches!(
            &events[6],
            StreamEvent::MessageDelta {
                stop_reason: Some(reason)
            } if reason == "tool_use"
        ));
    }

    #[tokio::test]
    async fn test_sse_parser_error_response() {
        let stream = mock_byte_stream(SSE_ERROR_RESPONSE);
        let mut parser = SseParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.expect("Expected valid event"));
        }

        // Verify we got the error event
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[1],
            StreamEvent::Error {
                error_type: "overloaded_error".to_string(),
                message: "API is temporarily overloaded".to_string()
            }
        );
    }

    #[tokio::test]
    async fn test_sse_parser_handles_incomplete_chunks() {
        // Simulate receiving data in very small chunks that split across event boundaries
        let data = r#"event: ping
data: {"type":"ping"}

event: message_stop
data: {"type":"message_stop"}

"#;
        let chunks: Vec<std::result::Result<bytes::Bytes, std::io::Error>> = data
            .as_bytes()
            .chunks(10) // Very small chunks
            .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
            .collect();
        let stream = futures_util::stream::iter(chunks);
        let mut parser = SseParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.expect("Expected valid event"));
        }

        assert_eq!(events.len(), 2);
        assert_eq!(events[0], StreamEvent::Ping);
        assert_eq!(events[1], StreamEvent::MessageStop);
    }

    #[tokio::test]
    async fn test_sse_parser_handles_utf8_split_across_chunks() {
        // Test that multi-byte UTF-8 characters split across TCP chunks are handled correctly.
        // 👋 = F0 9F 91 8B (4 bytes) - splitting this would corrupt with from_utf8_lossy
        let data = r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello 👋 world"}}

"#;
        let bytes = data.as_bytes();

        // Find the start of the emoji (F0 byte) and split inside it
        let emoji_start = bytes
            .windows(4)
            .position(|w| w == [0xF0, 0x9F, 0x91, 0x8B])
            .expect("emoji not found");

        // Split right in the middle of the emoji (after 2 of 4 bytes)
        let split_point = emoji_start + 2;

        let chunks: Vec<std::result::Result<bytes::Bytes, std::io::Error>> = vec![
            Ok(bytes::Bytes::copy_from_slice(&bytes[..split_point])),
            Ok(bytes::Bytes::copy_from_slice(&bytes[split_point..])),
        ];

        let stream = futures_util::stream::iter(chunks);
        let mut parser = SseParser::new(stream);

        let event = parser
            .next()
            .await
            .unwrap()
            .expect("should parse valid event");

        // Verify the emoji is intact, not corrupted with replacement characters
        assert_eq!(
            event,
            StreamEvent::TextDelta {
                index: 0,
                text: "Hello 👋 world".to_string()
            }
        );
    }
}
