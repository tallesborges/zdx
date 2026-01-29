//! OpenAI-compatible Chat Completions helpers.

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;

use anyhow::Result;
use eventsource_stream::{EventStream, Eventsource};
use futures_util::Stream;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Serialize;
use serde_json::Value;

use crate::providers::debug_metrics::maybe_wrap_with_metrics;
use crate::providers::{
    ChatContentBlock, ChatMessage, ContentBlockType, MessageContent, ProviderError,
    ProviderErrorKind, ProviderResult, ProviderStream, StreamEvent, Usage,
};
use crate::tools::{ToolDefinition, ToolResult, ToolResultContent};

const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";

/// OpenAI-compatible chat completions configuration.
#[derive(Debug, Clone)]
pub struct OpenAIChatCompletionsConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub reasoning_effort: Option<String>,
    pub extra_headers: HeaderMap,
    pub include_usage: bool,
    pub include_reasoning_content: bool,
}

/// OpenAI-compatible chat completions client.
pub struct OpenAIChatCompletionsClient {
    config: OpenAIChatCompletionsConfig,
    http: reqwest::Client,
}

impl OpenAIChatCompletionsClient {
    pub fn new(config: OpenAIChatCompletionsConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let request = ChatCompletionRequest::new(&self.config, messages, tools, system)?;

        let url = format!("{}{}", self.config.base_url, CHAT_COMPLETIONS_PATH);
        let headers = build_headers(&self.config.api_key, &self.config.extra_headers);

        let response = self
            .http
            .post(&url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .map_err(classify_reqwest_error)?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::http_status(status.as_u16(), &error_body).into());
        }

        let byte_stream = response.bytes_stream();
        let event_stream = ChatCompletionsSseParser::new(byte_stream, self.config.model.clone());
        Ok(maybe_wrap_with_metrics(event_stream))
    }
}

fn build_headers(api_key: &str, extra_headers: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", api_key))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));

    for (name, value) in extra_headers.iter() {
        headers.insert(name, value.clone());
    }

    headers
}

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

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    stream: bool,
    messages: Vec<ChatCompletionMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ChatToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningConfig>,
}

#[derive(Debug, Serialize)]
struct ReasoningConfig {
    effort: String,
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct ChatCompletionMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<ChatMessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

/// Message content - either a simple string or an array of content parts.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ChatMessageContent {
    /// Simple text content (serializes as a string)
    Text(String),
    /// Multi-part content with text and images (serializes as an array)
    Parts(Vec<ChatContentPart>),
}

/// Content part for multi-part messages.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ChatContentPart {
    /// Text content part
    Text { text: String },
    /// Image URL content part (supports both URLs and base64 data URLs)
    ImageUrl { image_url: ImageUrlData },
}

/// Image URL data structure.
#[derive(Debug, Serialize)]
struct ImageUrlData {
    url: String,
}

#[derive(Debug, Serialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: ChatToolCallFunction,
}

#[derive(Debug, Serialize)]
struct ChatToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct ChatToolDefinition {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: ChatToolFunction,
}

#[derive(Debug, Serialize)]
struct ChatToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

impl From<&ToolDefinition> for ChatToolDefinition {
    fn from(tool: &ToolDefinition) -> Self {
        // Use lowercase tool names for OpenAI-compatible chat completions.
        let tool = tool.with_lowercase_name();
        Self {
            tool_type: "function",
            function: ChatToolFunction {
                name: tool.name,
                description: tool.description,
                parameters: tool.input_schema,
            },
        }
    }
}

impl ChatCompletionRequest {
    fn new(
        config: &OpenAIChatCompletionsConfig,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<Self> {
        let mut out_messages = Vec::new();

        if let Some(prompt) = system
            && !prompt.trim().is_empty()
        {
            out_messages.push(ChatCompletionMessage {
                role: "system".to_string(),
                content: Some(ChatMessageContent::Text(prompt.to_string())),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            });
        }

        for msg in messages {
            match (&msg.role[..], &msg.content) {
                ("user", MessageContent::Text(text)) => {
                    out_messages.push(ChatCompletionMessage {
                        role: "user".to_string(),
                        content: Some(ChatMessageContent::Text(text.clone())),
                        reasoning_content: None,
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                ("assistant", MessageContent::Text(text)) => {
                    out_messages.push(ChatCompletionMessage {
                        role: "assistant".to_string(),
                        content: Some(ChatMessageContent::Text(text.clone())),
                        reasoning_content: None,
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                ("assistant", MessageContent::Blocks(blocks)) => {
                    let mut text = String::new();
                    let mut reasoning_content = String::new();
                    let mut tool_calls = Vec::new();

                    for block in blocks {
                        match block {
                            ChatContentBlock::Text(value) => text.push_str(value),
                            ChatContentBlock::ToolUse { id, name, input } => {
                                let args = serde_json::to_string(input)
                                    .unwrap_or_else(|_| "{}".to_string());
                                tool_calls.push(ChatToolCall {
                                    id: id.clone(),
                                    tool_type: "function",
                                    function: ChatToolCallFunction {
                                        name: name.clone(),
                                        arguments: args,
                                    },
                                });
                            }
                            ChatContentBlock::Reasoning(reasoning) => {
                                if config.include_reasoning_content
                                    && let Some(text) = &reasoning.text
                                {
                                    reasoning_content.push_str(text);
                                }
                            }
                            ChatContentBlock::ToolResult(_) => {}
                            // Assistant images are not supported in chat-completions payloads.
                            ChatContentBlock::Image { .. } => {}
                        }
                    }

                    if text.is_empty() && tool_calls.is_empty() && reasoning_content.is_empty() {
                        continue;
                    }

                    out_messages.push(ChatCompletionMessage {
                        role: "assistant".to_string(),
                        content: (!text.is_empty()).then_some(ChatMessageContent::Text(text)),
                        reasoning_content: (!reasoning_content.is_empty())
                            .then_some(reasoning_content),
                        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
                        tool_call_id: None,
                    });
                }
                ("user", MessageContent::Blocks(blocks)) => {
                    let mut content_parts: Vec<ChatContentPart> = Vec::new();
                    let mut tool_results: Vec<&ToolResult> = Vec::new();

                    for block in blocks {
                        match block {
                            ChatContentBlock::Text(value) => {
                                content_parts.push(ChatContentPart::Text {
                                    text: value.clone(),
                                });
                            }
                            ChatContentBlock::Image { mime_type, data } => {
                                // Convert to data URL format
                                let url = format!("data:{};base64,{}", mime_type, data);
                                content_parts.push(ChatContentPart::ImageUrl {
                                    image_url: ImageUrlData { url },
                                });
                            }
                            ChatContentBlock::ToolResult(result) => tool_results.push(result),
                            _ => {}
                        }
                    }

                    // Add user message with content parts (text and images)
                    if !content_parts.is_empty() {
                        // If only text parts (no images), collapse to simple string for compatibility
                        // with non-multimodal models and legacy OpenAI-compatible endpoints
                        let has_images = content_parts
                            .iter()
                            .any(|p| matches!(p, ChatContentPart::ImageUrl { .. }));

                        let content = if !has_images && content_parts.len() == 1 {
                            // Single text part - use string format
                            if let ChatContentPart::Text { text } = &content_parts[0] {
                                ChatMessageContent::Text(text.clone())
                            } else {
                                ChatMessageContent::Parts(content_parts)
                            }
                        } else if !has_images {
                            // Multiple text parts - concatenate into single string
                            let combined: String = content_parts
                                .iter()
                                .filter_map(|p| match p {
                                    ChatContentPart::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("");
                            ChatMessageContent::Text(combined)
                        } else {
                            // Has images - use array format
                            ChatMessageContent::Parts(content_parts)
                        };

                        out_messages.push(ChatCompletionMessage {
                            role: "user".to_string(),
                            content: Some(content),
                            reasoning_content: None,
                            tool_calls: None,
                            tool_call_id: None,
                        });
                    }

                    // Add tool results as separate tool messages
                    for result in tool_results {
                        // Extract text and optional image from tool result
                        let (text, image) = extract_tool_result_with_image(&result.content);

                        out_messages.push(ChatCompletionMessage {
                            role: "tool".to_string(),
                            content: Some(ChatMessageContent::Text(text)),
                            reasoning_content: None,
                            tool_calls: None,
                            tool_call_id: Some(result.tool_use_id.clone()),
                        });

                        // If there's an image in the tool result, add it as a follow-up user message
                        // (OpenAI-compatible chat completions don't support images in tool responses)
                        if let Some((mime_type, data)) = image {
                            let url = format!("data:{};base64,{}", mime_type, data);
                            out_messages.push(ChatCompletionMessage {
                                role: "user".to_string(),
                                content: Some(ChatMessageContent::Parts(vec![
                                    ChatContentPart::ImageUrl {
                                        image_url: ImageUrlData { url },
                                    },
                                ])),
                                reasoning_content: None,
                                tool_calls: None,
                                tool_call_id: None,
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        let tool_defs = if tools.is_empty() {
            None
        } else {
            Some(tools.iter().map(ChatToolDefinition::from).collect())
        };

        let stream_options = config.include_usage.then_some(StreamOptions {
            include_usage: true,
        });

        Ok(Self {
            model: config.model.clone(),
            stream: true,
            messages: out_messages,
            tools: tool_defs,
            max_tokens: config.max_tokens,
            stream_options,
            reasoning: config
                .reasoning_effort
                .clone()
                .map(|effort| ReasoningConfig { effort }),
        })
    }
}

/// Extracts text and optional image from tool result content.
/// Returns (text, Option<(mime_type, base64_data)>)
fn extract_tool_result_with_image(
    content: &ToolResultContent,
) -> (String, Option<(String, String)>) {
    match content {
        ToolResultContent::Text(text) => (text.clone(), None),
        ToolResultContent::Blocks(blocks) => {
            let text = blocks
                .iter()
                .find_map(|block| match block {
                    crate::tools::ToolResultBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();

            let image = blocks.iter().find_map(|block| match block {
                crate::tools::ToolResultBlock::Image { mime_type, data } => {
                    Some((mime_type.clone(), data.clone()))
                }
                _ => None,
            });

            (text, image)
        }
    }
}

#[derive(Debug)]
struct ToolCallState {
    stream_index: usize,
}

struct SseTerminatedStream<S> {
    inner: S,
    emitted_terminator: bool,
}

impl<S> SseTerminatedStream<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            emitted_terminator: false,
        }
    }
}

impl<S, E> Stream for SseTerminatedStream<S>
where
    S: Stream<Item = std::result::Result<bytes::Bytes, E>> + Unpin,
{
    type Item = std::result::Result<bytes::Bytes, E>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;

        if self.emitted_terminator {
            return Poll::Ready(None);
        }

        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(item)),
            Poll::Ready(None) => {
                self.emitted_terminator = true;
                Poll::Ready(Some(Ok(bytes::Bytes::from_static(b"\n\n"))))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// SSE parser for OpenAI-compatible chat completions.
struct ChatCompletionsSseParser<S> {
    inner: EventStream<SseTerminatedStream<S>>,
    model: String,
    pending: VecDeque<StreamEvent>,
    next_index: usize,
    text_index: Option<usize>,
    reasoning_index: Option<usize>,
    saw_tool: bool,
    tool_calls: HashMap<u32, ToolCallState>,
    final_usage: Option<Usage>,
    final_finish_reason: Option<String>,
    emitted_done: bool,
}

impl<S> ChatCompletionsSseParser<S> {
    fn new<E>(stream: S, model: String) -> Self
    where
        S: Stream<Item = std::result::Result<bytes::Bytes, E>> + Unpin,
    {
        Self {
            inner: SseTerminatedStream::new(stream).eventsource(),
            model,
            pending: VecDeque::new(),
            next_index: 0,
            text_index: None,
            reasoning_index: None,
            saw_tool: false,
            tool_calls: HashMap::new(),
            final_usage: None,
            final_finish_reason: None,
            emitted_done: false,
        }
    }

    /// Emit completion events. Called either when we have both finish_reason + usage,
    /// or when the stream ends (force=true).
    fn emit_completion_if_pending(&mut self, force: bool) {
        if self.emitted_done {
            return;
        }

        // In normal mode, require finish_reason. In force mode (stream end), emit anyway.
        let reason = match &self.final_finish_reason {
            Some(r) => r.clone(),
            None if force => "stop".to_string(), // Default stop reason on stream end
            None => return,
        };

        self.emitted_done = true;

        if let Some(index) = self.text_index.take() {
            self.pending
                .push_back(StreamEvent::ContentBlockCompleted { index });
        }

        if let Some(index) = self.reasoning_index.take() {
            self.pending
                .push_back(StreamEvent::ContentBlockCompleted { index });
        }

        let tool_indices: Vec<usize> = self
            .tool_calls
            .values()
            .map(|state| state.stream_index)
            .collect();
        for index in tool_indices {
            self.pending
                .push_back(StreamEvent::ContentBlockCompleted { index });
        }

        let usage = self.final_usage.clone().unwrap_or_default();
        let stop_reason = if self.saw_tool || reason == "tool_calls" {
            Some("tool_use".to_string())
        } else {
            Some(map_finish_reason(&reason))
        };

        self.pending.push_back(StreamEvent::MessageStart {
            model: self.model.clone(),
            usage: usage.clone(),
        });
        self.pending.push_back(StreamEvent::MessageDelta {
            stop_reason,
            usage: Some(usage),
        });
        self.pending.push_back(StreamEvent::MessageCompleted);
    }

    fn handle_event_data(&mut self, data: &str) -> ProviderResult<()> {
        let trimmed = data.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            return Ok(());
        }

        let value = serde_json::from_str::<Value>(trimmed).map_err(|err| {
            ProviderError::new(
                ProviderErrorKind::Parse,
                format!("Failed to parse SSE JSON: {}", err),
            )
        })?;
        self.handle_chunk(value)
    }

    fn handle_chunk(&mut self, value: Value) -> ProviderResult<()> {
        // Handle errors first - these are terminal, no completion should follow
        if let Some(error) = value.get("error") {
            let error_type = error
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("error")
                .to_string();
            let message = error
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            self.pending.push_back(StreamEvent::Error {
                error_type,
                message,
            });
            // Mark as done to prevent completion events after error
            self.emitted_done = true;
            return Ok(());
        }

        // Parse choices first (may be absent in usage-only chunks)
        let first_choice = value
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first());

        if let Some(choice) = first_choice {
            // Extract finish_reason if present
            if let Some(finish_reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                self.final_finish_reason = Some(finish_reason.to_string());
            }

            // Process delta content
            if let Some(delta) = choice.get("delta") {
                self.process_delta(delta);
            }
        }

        // Parse usage (can arrive in any chunk, often in a separate final chunk)
        // Check root level first (standard OpenAI), then choice level (Moonshot/Kimi)
        let usage_value = value.get("usage").or_else(|| first_choice?.get("usage"));
        if let Some(usage) = usage_value {
            self.final_usage = Some(parse_usage(usage));
        }

        // Emit completion when we have BOTH finish_reason AND usage
        // (OpenAI-compatible providers may send usage in a separate chunk after finish_reason when
        // stream_options.include_usage is true)
        if self.final_finish_reason.is_some() && self.final_usage.is_some() && !self.emitted_done {
            self.emit_completion_if_pending(false);
        }

        Ok(())
    }

    fn process_delta(&mut self, delta: &Value) {
        // Handle text content
        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
            if self.text_index.is_none() {
                let index = self.next_index;
                self.next_index += 1;
                self.text_index = Some(index);
                self.pending.push_back(StreamEvent::ContentBlockStart {
                    index,
                    block_type: ContentBlockType::Text,
                    id: None,
                    name: None,
                });
            }
            if !text.is_empty() {
                self.pending.push_back(StreamEvent::TextDelta {
                    index: self.text_index.unwrap_or(0),
                    text: text.to_string(),
                });
            }
        }

        // Handle reasoning content (Moonshot/Kimi and other OpenAI-compatible providers)
        if let Some(reasoning) = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(|v| v.as_str())
        {
            if self.reasoning_index.is_none() {
                let index = self.next_index;
                self.next_index += 1;
                self.reasoning_index = Some(index);
                self.pending.push_back(StreamEvent::ContentBlockStart {
                    index,
                    block_type: ContentBlockType::Reasoning,
                    id: None,
                    name: None,
                });
            }
            if !reasoning.is_empty() {
                self.pending.push_back(StreamEvent::ReasoningDelta {
                    index: self.reasoning_index.unwrap_or(0),
                    reasoning: reasoning.to_string(),
                });
            }
        }

        // Handle tool calls
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tool_call in tool_calls {
                let idx = tool_call.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let id = tool_call.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let function = tool_call.get("function").unwrap_or(&Value::Null);
                let name = function.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = function
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let entry = self.tool_calls.entry(idx).or_insert_with(|| {
                    let stream_index = self.next_index;
                    self.next_index += 1;
                    let tool_id = if id.is_empty() {
                        format!("toolcall-{}", idx)
                    } else {
                        id.to_string()
                    };
                    let name = if name.is_empty() {
                        "".to_string()
                    } else {
                        name.to_string()
                    };
                    self.saw_tool = true;
                    self.pending.push_back(StreamEvent::ContentBlockStart {
                        index: stream_index,
                        block_type: ContentBlockType::ToolUse,
                        id: Some(tool_id.clone()),
                        name: Some(name.clone()),
                    });
                    ToolCallState { stream_index }
                });

                if !args.is_empty() {
                    self.pending.push_back(StreamEvent::InputJsonDelta {
                        index: entry.stream_index,
                        partial_json: args.to_string(),
                    });
                }
            }
        }
    }
}

impl<S, E> Stream for ChatCompletionsSseParser<S>
where
    S: Stream<Item = std::result::Result<bytes::Bytes, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
{
    type Item = ProviderResult<StreamEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;

        loop {
            if let Some(event) = self.pending.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }

            let inner = Pin::new(&mut self.inner);
            match inner.poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => {
                    if let Err(err) = self.handle_event_data(&event.data) {
                        return Poll::Ready(Some(Err(err)));
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(ProviderError::new(
                        ProviderErrorKind::Parse,
                        format!("SSE stream error: {}", e),
                    ))));
                }
                Poll::Ready(None) => {
                    // Stream ended - force emit completion if we haven't yet
                    // (handles providers that don't send usage data or finish_reason)
                    self.emit_completion_if_pending(true);
                    if let Some(event) = self.pending.pop_front() {
                        return Poll::Ready(Some(Ok(event)));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

fn map_finish_reason(reason: &str) -> String {
    match reason {
        "length" => "max_tokens".to_string(),
        "content_filter" => "error".to_string(),
        other => other.to_string(),
    }
}

/// Parse usage from a JSON value.
/// Handles both standard OpenAI format and provider-specific variations (e.g., Moonshot/Kimi's `cached_tokens`).
fn parse_usage(usage: &Value) -> Usage {
    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Parse cached tokens - different providers use different field names:
    // - `cached_tokens` (Moonshot/Kimi)
    // - `prompt_tokens_details.cached_tokens` (OpenAI)
    let cached_tokens = usage
        .get("cached_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
        })
        .unwrap_or(0);

    Usage {
        input_tokens: prompt_tokens,
        output_tokens: completion_tokens,
        cache_read_input_tokens: cached_tokens,
        cache_creation_input_tokens: 0,
    }
}
