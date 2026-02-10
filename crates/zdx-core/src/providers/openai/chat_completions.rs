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
    ChatContentBlock, ChatMessage, ContentBlockType, DebugTrace, MessageContent, ProviderError,
    ProviderErrorKind, ProviderResult, ProviderStream, StreamEvent, Usage, wrap_stream,
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
    pub max_completion_tokens: Option<u32>,
    pub reasoning_effort: Option<String>,
    pub prompt_cache_key: Option<String>,
    pub extra_headers: HeaderMap,
    pub include_usage: bool,
    pub include_reasoning_content: bool,
    pub thinking: Option<ThinkingConfig>,
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

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let request = ChatCompletionRequest::new(&self.config, messages, tools, system);
        let trace =
            DebugTrace::from_env(&self.config.model, self.config.prompt_cache_key.as_deref());

        let url = format!("{}{}", self.config.base_url, CHAT_COMPLETIONS_PATH);
        let headers = build_headers(&self.config.api_key, &self.config.extra_headers);

        let response = if let Some(trace) = &trace {
            let body = serde_json::to_vec(&request)?;
            trace.write_request(&body);
            self.http
                .post(&url)
                .headers(headers)
                .body(body)
                .send()
                .await
                .map_err(|e| classify_reqwest_error(&e))?
        } else {
            self.http
                .post(&url)
                .headers(headers)
                .json(&request)
                .send()
                .await
                .map_err(|e| classify_reqwest_error(&e))?
        };

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::http_status(status.as_u16(), &error_body).into());
        }

        let byte_stream = wrap_stream(trace, response.bytes_stream());
        let event_stream = ChatCompletionsSseParser::new(byte_stream, self.config.model.clone());
        Ok(maybe_wrap_with_metrics(event_stream))
    }
}

fn build_headers(api_key: &str, extra_headers: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {api_key}"))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "user-agent",
        HeaderValue::from_static(crate::providers::shared::USER_AGENT),
    );

    for (name, value) in extra_headers {
        headers.insert(name, value.clone());
    }

    headers
}

fn classify_reqwest_error(e: &reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::timeout(format!("Request timed out: {e}"))
    } else if e.is_connect() {
        ProviderError::timeout(format!("Connection failed: {e}"))
    } else if e.is_request() {
        ProviderError::new(ProviderErrorKind::HttpStatus, format!("Request error: {e}"))
    } else {
        ProviderError::new(ProviderErrorKind::HttpStatus, format!("Network error: {e}"))
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
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReasoningConfig {
    effort: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub kind: String,
}

impl From<bool> for ThinkingConfig {
    fn from(enabled: bool) -> Self {
        if enabled {
            Self {
                kind: "enabled".to_string(),
            }
        } else {
            Self {
                kind: "disabled".to_string(),
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct ChatCompletionMessage {
    role: String,
    // NOTE: We intentionally do NOT use skip_serializing_if here.
    // Prefix caching requires byte-for-byte alignment of the full context.
    // If content is None, we must serialize it as "content":null, not omit it.
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
    ) -> Self {
        let mut out_messages = Vec::new();
        push_system_message(system, &mut out_messages);
        for msg in messages {
            append_chat_message(config, msg, &mut out_messages);
        }

        Self {
            model: config.model.clone(),
            stream: true,
            messages: out_messages,
            tools: (!tools.is_empty()).then(|| tools.iter().map(ChatToolDefinition::from).collect()),
            max_tokens: config.max_tokens,
            max_completion_tokens: config.max_completion_tokens,
            stream_options: config.include_usage.then_some(StreamOptions { include_usage: true }),
            reasoning: config
                .reasoning_effort
                .clone()
                .map(|effort| ReasoningConfig { effort }),
            thinking: config.thinking.clone(),
            prompt_cache_key: config.prompt_cache_key.clone(),
        }
    }
}

fn push_system_message(system: Option<&str>, out_messages: &mut Vec<ChatCompletionMessage>) {
    if let Some(prompt) = system
        && !prompt.trim().is_empty()
    {
        out_messages.push(simple_message(
            "system",
            ChatMessageContent::Text(prompt.to_string()),
        ));
    }
}

fn append_chat_message(
    config: &OpenAIChatCompletionsConfig,
    msg: &ChatMessage,
    out_messages: &mut Vec<ChatCompletionMessage>,
) {
    match (&msg.role[..], &msg.content) {
        ("user", MessageContent::Text(text)) => {
            out_messages.push(simple_message("user", ChatMessageContent::Text(text.clone())));
        }
        ("assistant", MessageContent::Text(text)) => {
            out_messages.push(simple_message(
                "assistant",
                ChatMessageContent::Text(text.clone()),
            ));
        }
        ("assistant", MessageContent::Blocks(blocks)) => {
            if let Some(message) = assistant_blocks_message(blocks, config.include_reasoning_content) {
                out_messages.push(message);
            }
        }
        ("user", MessageContent::Blocks(blocks)) => {
            out_messages.extend(user_blocks_messages(blocks));
        }
        _ => {}
    }
}

fn simple_message(role: &str, content: ChatMessageContent) -> ChatCompletionMessage {
    ChatCompletionMessage {
        role: role.to_string(),
        content: Some(content),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
    }
}

fn assistant_blocks_message(
    blocks: &[ChatContentBlock],
    include_reasoning_content: bool,
) -> Option<ChatCompletionMessage> {
    let mut text = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls = Vec::new();

    for block in blocks {
        match block {
            ChatContentBlock::Text(value) => text.push_str(value),
            ChatContentBlock::ToolUse { id, name, input } => {
                let arguments = serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string());
                tool_calls.push(ChatToolCall {
                    id: id.clone(),
                    tool_type: "function",
                    function: ChatToolCallFunction {
                        name: name.clone(),
                        arguments,
                    },
                });
            }
            ChatContentBlock::Reasoning(reasoning) => {
                if include_reasoning_content
                    && let Some(text) = &reasoning.text
                {
                    reasoning_content.push_str(text);
                }
            }
            ChatContentBlock::ToolResult(_) | ChatContentBlock::Image { .. } => {}
        }
    }

    if text.is_empty() && tool_calls.is_empty() && reasoning_content.is_empty() {
        return None;
    }

    Some(ChatCompletionMessage {
        role: "assistant".to_string(),
        content: (!text.is_empty()).then_some(ChatMessageContent::Text(text)),
        reasoning_content: (!reasoning_content.is_empty()).then_some(reasoning_content),
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        tool_call_id: None,
    })
}

fn user_blocks_messages(blocks: &[ChatContentBlock]) -> Vec<ChatCompletionMessage> {
    let mut messages = Vec::new();
    let mut content_parts = Vec::new();
    let mut tool_results: Vec<&ToolResult> = Vec::new();

    for block in blocks {
        match block {
            ChatContentBlock::Text(value) => {
                content_parts.push(ChatContentPart::Text {
                    text: value.clone(),
                });
            }
            ChatContentBlock::Image { mime_type, data } => {
                let url = format!("data:{mime_type};base64,{data}");
                content_parts.push(ChatContentPart::ImageUrl {
                    image_url: ImageUrlData { url },
                });
            }
            ChatContentBlock::ToolResult(result) => tool_results.push(result),
            _ => {}
        }
    }

    if !content_parts.is_empty() {
        messages.push(simple_message(
            "user",
            collapse_user_content_parts(content_parts),
        ));
    }

    for result in tool_results {
        let (text, image) = extract_tool_result_with_image(&result.content);
        messages.push(ChatCompletionMessage {
            role: "tool".to_string(),
            content: Some(ChatMessageContent::Text(text)),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(result.tool_use_id.clone()),
        });

        if let Some((mime_type, data)) = image {
            let url = format!("data:{mime_type};base64,{data}");
            messages.push(simple_message(
                "user",
                ChatMessageContent::Parts(vec![ChatContentPart::ImageUrl {
                    image_url: ImageUrlData { url },
                }]),
            ));
        }
    }

    messages
}

fn collapse_user_content_parts(content_parts: Vec<ChatContentPart>) -> ChatMessageContent {
    let has_images = content_parts
        .iter()
        .any(|part| matches!(part, ChatContentPart::ImageUrl { .. }));
    if has_images {
        return ChatMessageContent::Parts(content_parts);
    }

    let mut text_parts = content_parts.iter().filter_map(|part| match part {
        ChatContentPart::Text { text } => Some(text.as_str()),
        ChatContentPart::ImageUrl { .. } => None,
    });
    let Some(first) = text_parts.next() else {
        return ChatMessageContent::Parts(content_parts);
    };
    let Some(second) = text_parts.next() else {
        return ChatMessageContent::Text(first.to_string());
    };

    let mut combined = String::with_capacity(first.len() + second.len());
    combined.push_str(first);
    combined.push_str(second);
    for text in text_parts {
        combined.push_str(text);
    }
    ChatMessageContent::Text(combined)
}

/// Extracts text and optional image from tool result content.
/// Returns (text, Option<(`mime_type`, `base64_data`)>)
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
                    crate::tools::ToolResultBlock::Image { .. } => None,
                })
                .unwrap_or_default();

            let image = blocks.iter().find_map(|block| match block {
                crate::tools::ToolResultBlock::Image { mime_type, data } => {
                    Some((mime_type.clone(), data.clone()))
                }
                crate::tools::ToolResultBlock::Text { .. } => None,
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

    /// Emit completion events. Called either when we have both `finish_reason` + usage,
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
        if trimmed.is_empty() {
            return Ok(());
        }
        if trimmed == "[DONE]" {
            self.emit_completion_if_pending(true);
            return Ok(());
        }

        let value = serde_json::from_str::<Value>(trimmed).map_err(|err| {
            ProviderError::new(
                ProviderErrorKind::Parse,
                format!("Failed to parse SSE JSON: {err}"),
            )
        })?;
        self.handle_chunk(&value);
        Ok(())
    }
    fn handle_chunk(&mut self, value: &Value) {
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
            return;
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
        let usage_value = value
            .get("usage")
            .or_else(|| first_choice.and_then(|choice| choice.get("usage")));
        if let Some(usage) = usage_value {
            self.final_usage = Some(parse_usage(usage));

            // Some providers (e.g., MiMo/OpenAI) send a usage-only chunk with empty choices.
            // Treat this as end-of-stream and emit completion using the latest usage.
            let choices_empty = value
                .get("choices")
                .and_then(|v| v.as_array())
                .is_some_and(std::vec::Vec::is_empty);
            if choices_empty && !self.emitted_done {
                self.emit_completion_if_pending(true);
            }
        }
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

        // Handle reasoning content (Moonshot/Kimi, StepFun and other OpenAI-compatible providers)
        match delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(|v| v.as_str())
        {
            Some(reasoning) if !reasoning.is_empty() => {
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
                self.pending.push_back(StreamEvent::ReasoningDelta {
                    index: self.reasoning_index.unwrap_or(0),
                    reasoning: reasoning.to_string(),
                });
            }
            _ => (),
        }

        // Handle tool calls
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tool_call in tool_calls {
                let idx = u32::try_from(
                    tool_call
                        .get("index")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                )
                .unwrap_or(u32::MAX);
                let id = tool_call.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let function = tool_call.get("function").unwrap_or(&Value::Null);
                let name = function.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = function
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Skip tool calls with empty name only if we haven't seen this index before.
                // In normal streaming, name appears only in the first delta; later deltas
                // carry only arguments with empty name. We must process those for existing entries.
                if name.is_empty() && !self.tool_calls.contains_key(&idx) {
                    continue;
                }

                let entry = self.tool_calls.entry(idx).or_insert_with(|| {
                    let stream_index = self.next_index;
                    self.next_index += 1;
                    let tool_id = if id.is_empty() {
                        format!("toolcall-{idx}")
                    } else {
                        id.to_string()
                    };
                    self.saw_tool = true;
                    self.pending.push_back(StreamEvent::ContentBlockStart {
                        index: stream_index,
                        block_type: ContentBlockType::ToolUse,
                        id: Some(tool_id.clone()),
                        name: Some(name.to_string()),
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
                        format!("SSE stream error: {e}"),
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
/// Handles both standard `OpenAI` format and provider-specific variations (e.g., Moonshot/Kimi's `cached_tokens`).
fn parse_usage(usage: &Value) -> Usage {
    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    // Parse cached tokens - different providers use different field names:
    // - `cached_tokens` (Moonshot/Kimi)
    // - `prompt_tokens_details.cached_tokens` (OpenAI)
    let cached_tokens = usage
        .get("cached_tokens")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(0);

    // prompt_tokens includes cached tokens for OpenAI-compatible APIs.
    // Convert to non-cached input tokens to avoid double-counting in cost.
    let non_cached_input = prompt_tokens.saturating_sub(cached_tokens);

    Usage {
        input_tokens: non_cached_input,
        output_tokens: completion_tokens,
        cache_read_input_tokens: cached_tokens,
        cache_creation_input_tokens: 0,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ContentBlockType, StreamEvent, parse_usage};

    #[test]
    fn test_parse_usage_subtracts_cached_tokens() {
        let usage = json!({
            "prompt_tokens": 100,
            "completion_tokens": 25,
            "cached_tokens": 40
        });
        let parsed = parse_usage(&usage);
        assert_eq!(parsed.input_tokens, 60);
        assert_eq!(parsed.cache_read_input_tokens, 40);
        assert_eq!(parsed.output_tokens, 25);
    }

    #[test]
    fn test_parse_usage_prompt_tokens_details_cached_tokens() {
        let usage = json!({
            "prompt_tokens": 5199,
            "completion_tokens": 11,
            "prompt_tokens_details": { "cached_tokens": 3 }
        });
        let parsed = parse_usage(&usage);
        assert_eq!(parsed.input_tokens, 5196);
        assert_eq!(parsed.cache_read_input_tokens, 3);
        assert_eq!(parsed.output_tokens, 11);
    }

    /// Helper to parse SSE data and collect all events
    async fn parse_sse(sse_data: &str, model: &str) -> Vec<StreamEvent> {
        use futures_util::StreamExt;

        use super::ChatCompletionsSseParser;

        let stream = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(bytes::Bytes::from(
            sse_data.to_string(),
        ))]);
        let parser = ChatCompletionsSseParser::new(stream, model.to_string());
        parser.filter_map(|r| async { r.ok() }).collect().await
    }

    /// Helper to extract tool names from `ContentBlockStart` events
    fn extract_tool_names(events: &[StreamEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentBlockStart {
                    block_type: ContentBlockType::ToolUse,
                    name,
                    ..
                } => name.clone(),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn test_malformed_tool_call_ignored() {
        // Tool calls with empty 'name' fields should be ignored.
        // This prevents malformed/incomplete tool calls from providers like StepFun.
        let sse = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"t1","function":{"name":"","arguments":"{"}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"t2","function":{"name":"","arguments":""}}]}}]}

data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":100,"completion_tokens":10}}

data: [DONE]
"#;
        let events = parse_sse(sse, "test-model").await;

        assert!(
            extract_tool_names(&events).is_empty(),
            "Expected no tools for empty-name tool calls"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, StreamEvent::MessageCompleted)),
            "Expected MessageCompleted"
        );
    }

    #[tokio::test]
    async fn test_valid_tool_call_alongside_malformed() {
        // Valid tool calls should work even when malformed ones (empty name) are present.
        let sse = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"valid","function":{"name":"bash","arguments":"{\"command\":\"echo hi\"}"}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"bad","function":{"name":"","arguments":""}}]}}]}

data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":100,"completion_tokens":10}}

data: [DONE]
"#;
        let events = parse_sse(sse, "test-model").await;
        let tools = extract_tool_names(&events);

        assert_eq!(tools, vec!["bash"], "Expected only the valid 'bash' tool");
    }

    #[tokio::test]
    async fn test_multi_delta_tool_call_accumulates_arguments() {
        // In OpenAI streaming, name appears only in first delta; later deltas have empty name.
        // Arguments should accumulate correctly across all deltas.
        let sse = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"t1","function":{"name":"bash","arguments":""}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"","arguments":"{\"com"}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"","arguments":"mand\":\""}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"","arguments":"echo hi\"}"}}]}}]}

data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":100,"completion_tokens":10}}

data: [DONE]
"#;
        let events = parse_sse(sse, "test-model").await;

        assert_eq!(extract_tool_names(&events), vec!["bash"]);

        let json: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::InputJsonDelta { partial_json, .. } => Some(partial_json.clone()),
                _ => None,
            })
            .collect();

        assert_eq!(json, r#"{"command":"echo hi"}"#);
    }
}
