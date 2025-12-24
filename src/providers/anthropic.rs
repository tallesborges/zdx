//! Anthropic Claude API client.
//!
//! # Prompt Caching Strategy
//!
//! Anthropic allows up to 4 cache breakpoints per request. Each breakpoint caches
//! everything from the START of the request up to that marker (prefix caching).
//! Minimum cache size is 1,024 tokens.
//!
//! We use 2 breakpoints:
//! - **BP1 (last system block)**: Caches system prompt + AGENTS.md context.
//!   Reused across sessions with the same config.
//! - **BP2 (last user message)**: Caches conversation history.
//!   Reused within the same session for subsequent turns.
//!
//! This ensures the large system prompt is cached even for short conversations,
//! and provides cross-session cache hits when starting new conversations.

use std::fmt;
use std::pin::Pin;

use anyhow::{Context, Result, bail};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::{ToolDefinition, ToolResult};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
/// Beta features for API key authentication
const BETA_HEADER: &str = "fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14";
/// Beta features for OAuth authentication
const OAUTH_BETA_HEADER: &str =
    "oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14";
/// Required system prompt prefix for OAuth tokens (Claude Code identification)
const CLAUDE_CODE_SYSTEM_PROMPT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

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
    /// Creates an HTTP status error.
    pub fn http_status(status: u16, body: &str) -> Self {
        let message = format!("HTTP {}", status);
        let details = if body.is_empty() {
            None
        } else {
            // Try to extract a cleaner error message from JSON
            if let Ok(json) = serde_json::from_str::<Value>(body)
                && let Some(error_obj) = json.get("error")
                && let Some(msg) = error_obj.get("message").and_then(|v| v.as_str())
            {
                return Self {
                    kind: ProviderErrorKind::HttpStatus,
                    message: format!("HTTP {}: {}", status, msg),
                    details: Some(body.to_string()),
                };
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

/// Authentication type for Anthropic API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthType {
    /// API key authentication (uses `x-api-key` header)
    ApiKey,
    /// OAuth token authentication (uses `Authorization: Bearer` header)
    OAuth,
}

/// Configuration for the Anthropic client.
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    /// The authentication token (API key or OAuth access token)
    pub auth_token: String,
    /// The type of authentication
    pub auth_type: AuthType,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    /// Whether extended thinking is enabled
    pub thinking_enabled: bool,
    /// Token budget for thinking (only used when thinking_enabled = true)
    pub thinking_budget_tokens: u32,
}

impl AnthropicConfig {
    /// Creates a new config from environment and OAuth cache.
    ///
    /// Authentication resolution order:
    /// 1. OAuth token from `~/.zdx/oauth.json` (if present and valid)
    /// 2. `ANTHROPIC_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `ANTHROPIC_API_KEY`: API key (used if no OAuth token)
    /// - `ANTHROPIC_BASE_URL`: Optional base URL override
    ///
    /// Base URL resolution order:
    /// 1. `ANTHROPIC_BASE_URL` env var (if set and non-empty)
    /// 2. `config_base_url` parameter (if Some and non-empty)
    /// 3. Default: `https://api.anthropic.com`
    pub fn from_env(
        model: String,
        max_tokens: u32,
        config_base_url: Option<&str>,
        thinking_enabled: bool,
        thinking_budget_tokens: u32,
    ) -> Result<Self> {
        let (auth_token, auth_type) = Self::resolve_auth()?;

        // Resolution order: env > config > default
        let base_url = Self::resolve_base_url(config_base_url)?;

        Ok(Self {
            auth_token,
            auth_type,
            base_url,
            model,
            max_tokens,
            thinking_enabled,
            thinking_budget_tokens,
        })
    }

    /// Resolves authentication credentials.
    /// Precedence: OAuth token > ANTHROPIC_API_KEY
    fn resolve_auth() -> Result<(String, AuthType)> {
        use crate::providers::oauth::anthropic as oauth_anthropic;

        // Try OAuth token first
        match oauth_anthropic::load_credentials() {
            Ok(Some(creds)) => {
                if creds.is_expired() {
                    // Token expired, try to refresh synchronously
                    // Note: This blocks, but is acceptable at startup
                    let rt = tokio::runtime::Handle::try_current();
                    let refreshed = if let Ok(handle) = rt {
                        // We're already in a tokio context, spawn blocking
                        tokio::task::block_in_place(|| {
                            handle.block_on(oauth_anthropic::refresh_token(&creds.refresh))
                        })
                    } else {
                        // Not in tokio context, create a small runtime
                        tokio::runtime::Runtime::new()
                            .context("create runtime for token refresh")?
                            .block_on(oauth_anthropic::refresh_token(&creds.refresh))
                    };

                    match refreshed {
                        Ok(new_creds) => {
                            oauth_anthropic::save_credentials(&new_creds)?;
                            return Ok((new_creds.access, AuthType::OAuth));
                        }
                        Err(e) => {
                            // Refresh failed, clear credentials and fall through to API key
                            let _ = oauth_anthropic::clear_credentials();
                            eprintln!(
                                "OAuth token expired and refresh failed: {}. Falling back to ANTHROPIC_API_KEY.",
                                e
                            );
                        }
                    }
                } else {
                    // Token is valid
                    return Ok((creds.access, AuthType::OAuth));
                }
            }
            Ok(None) => {
                // No OAuth credentials, fall through to API key
            }
            Err(e) => {
                // Error loading OAuth cache, log and fall through
                eprintln!(
                    "Warning: Failed to load OAuth cache: {}. Using ANTHROPIC_API_KEY.",
                    e
                );
            }
        }

        // Fall back to API key
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .context("No authentication available. Either run `zdx login --anthropic` or set ANTHROPIC_API_KEY environment variable.")?;

        Ok((api_key, AuthType::ApiKey))
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

    /// Sends a conversation and returns an async stream of events.
    ///
    /// This enables chunk-by-chunk token streaming from the API.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        // Convert messages to API format.
        // Only the last content block of the last user message gets cache_control
        // to respect Anthropic's limit of 4 cache_control blocks total.
        let mut api_messages: Vec<ApiMessage> = messages
            .iter()
            .map(|m| ApiMessage::from_chat_message(m, false))
            .collect();

        // Add cache_control to the last content block of the last user message
        if let Some(last_user_msg) = api_messages.iter_mut().rev().find(|m| m.role == "user")
            && let ApiMessageContent::Blocks(blocks) = &mut last_user_msg.content
            && let Some(last_block) = blocks.last_mut()
        {
            match last_block {
                ApiContentBlock::Text { cache_control, .. } => {
                    *cache_control = Some(CacheControl::ephemeral());
                }
                ApiContentBlock::ToolResult { cache_control, .. } => {
                    *cache_control = Some(CacheControl::ephemeral());
                }
                _ => {}
            }
        }

        let tool_defs = if tools.is_empty() {
            None
        } else {
            Some(tools.iter().map(ApiToolDef::from).collect::<Vec<_>>())
        };

        // Build system blocks based on auth type
        // OAuth requires the Claude Code system prompt prefix with cache_control
        let system_blocks = self.build_system_blocks(system);

        // Build thinking config if enabled
        let thinking = if self.config.thinking_enabled {
            Some(ThinkingConfig::enabled(self.config.thinking_budget_tokens))
        } else {
            None
        };

        let request = StreamingMessagesRequest {
            model: &self.config.model,
            max_tokens: self.config.max_tokens,
            messages: api_messages,
            tools: tool_defs,
            system: system_blocks,
            thinking,
            stream: true,
        };

        let url = format!("{}/v1/messages", self.config.base_url);

        let mut request_builder = self
            .http
            .post(&url)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "application/json");

        // Set authentication and beta headers based on auth type
        request_builder = match self.config.auth_type {
            AuthType::ApiKey => request_builder
                .header("x-api-key", &self.config.auth_token)
                .header("anthropic-beta", BETA_HEADER),
            AuthType::OAuth => request_builder
                .header(
                    "Authorization",
                    format!("Bearer {}", self.config.auth_token),
                )
                .header("anthropic-beta", OAUTH_BETA_HEADER),
        };

        let response = request_builder
            .json(&request)
            .send()
            .await
            .map_err(Self::classify_reqwest_error)?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::http_status(status.as_u16(), &error_body).into());
        }

        let byte_stream = response.bytes_stream();
        let event_stream = SseParser::new(byte_stream);
        Ok(Box::pin(event_stream))
    }

    /// Builds system message blocks with cache control on the last block only.
    ///
    /// Always includes the Claude Code identification prompt.
    /// Cache control placement:
    /// - Last system block: caches system prompt (often large with AGENTS.md)
    /// - Last user message: caches conversation history (set in send_messages_stream)
    ///
    /// This uses 2 of 4 allowed breakpoints. The minimum cache threshold is
    /// 1,024 tokens, so caching the system prompt separately ensures it gets
    /// cached even for short conversations.
    fn build_system_blocks(&self, system: Option<&str>) -> Option<Vec<SystemBlock>> {
        match system {
            Some(prompt) => Some(vec![
                SystemBlock::new(CLAUDE_CODE_SYSTEM_PROMPT),
                SystemBlock::with_cache_control(prompt),
            ]),
            None => Some(vec![SystemBlock::with_cache_control(
                CLAUDE_CODE_SYSTEM_PROMPT,
            )]),
        }
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

// === Streaming Types ===

/// Events emitted during streaming.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Message started, contains model info
    MessageStart { model: String },
    /// A content block has started (text or tool_use or thinking)
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
    /// Thinking delta within a thinking content block
    ThinkingDelta { index: usize, thinking: String },
    /// Signature delta within a thinking content block
    SignatureDelta { index: usize, signature: String },
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
                "thinking_delta" => Ok(StreamEvent::ThinkingDelta {
                    index: parsed.index,
                    thinking: parsed.delta.thinking.unwrap_or_default(),
                }),
                "signature_delta" => Ok(StreamEvent::SignatureDelta {
                    index: parsed.index,
                    signature: parsed.delta.signature.unwrap_or_default(),
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
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    signature: Option<String>,
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

/// Thinking configuration for extended thinking feature.
#[derive(Debug, Serialize)]
struct ThinkingConfig {
    #[serde(rename = "type")]
    thinking_type: &'static str,
    budget_tokens: u32,
}

impl ThinkingConfig {
    fn enabled(budget_tokens: u32) -> Self {
        Self {
            thinking_type: "enabled",
            budget_tokens,
        }
    }
}

#[derive(Debug, Serialize)]
struct StreamingMessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ApiToolDef<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<SystemBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
    stream: bool,
}

/// System message block with optional cache control.
#[derive(Debug, Serialize)]
struct SystemBlock {
    #[serde(rename = "type")]
    block_type: &'static str,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

impl SystemBlock {
    fn new(text: impl Into<String>) -> Self {
        Self {
            block_type: "text",
            text: text.into(),
            cache_control: None,
        }
    }

    fn with_cache_control(text: impl Into<String>) -> Self {
        Self {
            block_type: "text",
            text: text.into(),
            cache_control: Some(CacheControl::ephemeral()),
        }
    }
}

/// Cache control settings for prompt caching.
#[derive(Debug, Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    cache_type: &'static str,
}

impl CacheControl {
    fn ephemeral() -> Self {
        Self {
            cache_type: "ephemeral",
        }
    }
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
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
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
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

impl ApiMessage {
    /// Converts a ChatMessage to ApiMessage with optional cache control.
    fn from_chat_message(msg: &ChatMessage, use_cache_control: bool) -> Self {
        match &msg.content {
            MessageContent::Text(text) => ApiMessage {
                role: msg.role.clone(),
                content: ApiMessageContent::Text(text.clone()),
            },
            MessageContent::Blocks(blocks) => {
                let api_blocks: Vec<ApiContentBlock> = blocks
                    .iter()
                    .map(|b| match b {
                        ChatContentBlock::Text(text) => ApiContentBlock::Text {
                            text: text.clone(),
                            cache_control: if use_cache_control {
                                Some(CacheControl::ephemeral())
                            } else {
                                None
                            },
                        },
                        ChatContentBlock::ToolUse { id, name, input } => ApiContentBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        },
                        ChatContentBlock::ToolResult(result) => ApiContentBlock::ToolResult {
                            tool_use_id: result.tool_use_id.clone(),
                            content: result.content.clone(),
                            is_error: result.is_error,
                            cache_control: None,
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

// === Public Chat Types ===

/// Content block in a chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ChatContentBlock>),
}

/// A chat message with owned data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

    /// SSE fixture simulating a thinking response with interleaved thinking and text
    const SSE_THINKING_RESPONSE: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_think","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":" about this..."}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc123sig"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Here is my response."}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":50}}

event: message_stop
data: {"type":"message_stop"}

"#;

    #[tokio::test]
    async fn test_sse_parser_thinking_response() {
        let stream = mock_byte_stream(SSE_THINKING_RESPONSE);
        let mut parser = SseParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.expect("Expected valid event"));
        }

        // Verify we got all expected events
        assert_eq!(events.len(), 11);

        // Check message_start
        assert!(
            matches!(&events[0], StreamEvent::MessageStart { model } if model == "claude-sonnet-4-20250514")
        );

        // Check thinking block start
        assert!(matches!(
            &events[1],
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type,
                ..
            } if block_type == "thinking"
        ));

        // Check thinking deltas
        assert_eq!(
            events[2],
            StreamEvent::ThinkingDelta {
                index: 0,
                thinking: "Let me think".to_string()
            }
        );
        assert_eq!(
            events[3],
            StreamEvent::ThinkingDelta {
                index: 0,
                thinking: " about this...".to_string()
            }
        );

        // Check signature delta
        assert_eq!(
            events[4],
            StreamEvent::SignatureDelta {
                index: 0,
                signature: "abc123sig".to_string()
            }
        );

        // Check thinking block stop
        assert_eq!(events[5], StreamEvent::ContentBlockStop { index: 0 });

        // Check text block start
        assert!(matches!(
            &events[6],
            StreamEvent::ContentBlockStart {
                index: 1,
                block_type,
                ..
            } if block_type == "text"
        ));

        // Check text delta
        assert_eq!(
            events[7],
            StreamEvent::TextDelta {
                index: 1,
                text: "Here is my response.".to_string()
            }
        );

        // Check text block stop
        assert_eq!(events[8], StreamEvent::ContentBlockStop { index: 1 });

        // Check message delta and stop
        assert!(matches!(
            &events[9],
            StreamEvent::MessageDelta {
                stop_reason: Some(reason)
            } if reason == "end_turn"
        ));
        assert_eq!(events[10], StreamEvent::MessageStop);

        // Log actual events for debugging if needed
        // for (i, e) in events.iter().enumerate() {
        //     println!("{}: {:?}", i, e);
        // }
    }
}
