//! OpenRouter provider (OpenAI-compatible Chat Completions).

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;

use anyhow::{Context, Result, anyhow};
use futures_util::Stream;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Serialize;
use serde_json::Value;

use crate::providers::{
    ChatContentBlock, ChatMessage, MessageContent, ProviderError, ProviderErrorKind, StreamEvent,
    Usage,
};
use crate::tools::{ToolDefinition, ToolResult, ToolResultContent};

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";

/// OpenRouter API configuration.
#[derive(Debug, Clone)]
pub struct OpenRouterConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
}

impl OpenRouterConfig {
    /// Creates a new config from environment.
    ///
    /// Environment variables:
    /// - `OPENROUTER_API_KEY` (required)
    /// - `OPENROUTER_BASE_URL` (optional)
    /// - `OPENROUTER_SITE_URL` (optional)
    /// - `OPENROUTER_APP_NAME` (optional)
    pub fn from_env(model: String, max_tokens: u32, config_base_url: Option<&str>) -> Result<Self> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .context("OPENROUTER_API_KEY is not set. Set it to use OpenRouter.")?;
        let base_url = resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
        })
    }
}

/// OpenRouter client.
pub struct OpenRouterClient {
    config: OpenRouterConfig,
    http: reqwest::Client,
}

impl OpenRouterClient {
    pub fn new(config: OpenRouterConfig) -> Self {
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
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let request = ChatCompletionRequest::new(
            self.config.model.clone(),
            self.config.max_tokens,
            messages,
            tools,
            system,
        )?;

        let url = format!("{}{}", self.config.base_url, CHAT_COMPLETIONS_PATH);
        let headers = build_headers(&self.config.api_key);

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
        Ok(Box::pin(event_stream))
    }
}

fn resolve_base_url(config_base_url: Option<&str>) -> Result<String> {
    if let Ok(env_url) = std::env::var("OPENROUTER_BASE_URL") {
        let trimmed = env_url.trim();
        if !trimmed.is_empty() {
            validate_url(trimmed)?;
            return Ok(trimmed.to_string());
        }
    }

    if let Some(config_url) = config_base_url {
        let trimmed = config_url.trim();
        if !trimmed.is_empty() {
            validate_url(trimmed)?;
            return Ok(trimmed.to_string());
        }
    }

    Ok(DEFAULT_BASE_URL.to_string())
}

fn validate_url(url: &str) -> Result<()> {
    url::Url::parse(url).with_context(|| format!("Invalid OpenRouter base URL: {}", url))?;
    Ok(())
}

fn build_headers(api_key: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", api_key))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));

    if let Ok(site_url) = std::env::var("OPENROUTER_SITE_URL")
        && !site_url.trim().is_empty()
    {
        let _ = headers.insert(
            "HTTP-Referer",
            HeaderValue::from_str(site_url.trim()).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
    }
    if let Ok(app_name) = std::env::var("OPENROUTER_APP_NAME")
        && !app_name.trim().is_empty()
    {
        let _ = headers.insert(
            "X-Title",
            HeaderValue::from_str(app_name.trim()).unwrap_or_else(|_| HeaderValue::from_static("")),
        );
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
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct ChatCompletionMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
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
        Self {
            tool_type: "function",
            function: ChatToolFunction {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
            },
        }
    }
}

impl ChatCompletionRequest {
    fn new(
        model: String,
        max_tokens: u32,
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
                content: Some(prompt.to_string()),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        for msg in messages {
            match (&msg.role[..], &msg.content) {
                ("user", MessageContent::Text(text)) => {
                    out_messages.push(ChatCompletionMessage {
                        role: "user".to_string(),
                        content: Some(text.clone()),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                ("assistant", MessageContent::Text(text)) => {
                    out_messages.push(ChatCompletionMessage {
                        role: "assistant".to_string(),
                        content: Some(text.clone()),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                ("assistant", MessageContent::Blocks(blocks)) => {
                    let mut text = String::new();
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
                            ChatContentBlock::Thinking { .. } => {}
                            ChatContentBlock::ToolResult(_) => {}
                        }
                    }

                    if text.is_empty() && tool_calls.is_empty() {
                        continue;
                    }

                    out_messages.push(ChatCompletionMessage {
                        role: "assistant".to_string(),
                        content: (!text.is_empty()).then_some(text),
                        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
                        tool_call_id: None,
                    });
                }
                ("user", MessageContent::Blocks(blocks)) => {
                    let mut text = String::new();
                    let mut tool_results: Vec<&ToolResult> = Vec::new();

                    for block in blocks {
                        match block {
                            ChatContentBlock::Text(value) => text.push_str(value),
                            ChatContentBlock::ToolResult(result) => tool_results.push(result),
                            _ => {}
                        }
                    }

                    if !text.is_empty() {
                        out_messages.push(ChatCompletionMessage {
                            role: "user".to_string(),
                            content: Some(text),
                            tool_calls: None,
                            tool_call_id: None,
                        });
                    }

                    for result in tool_results {
                        out_messages.push(ChatCompletionMessage {
                            role: "tool".to_string(),
                            content: Some(tool_result_text(result)),
                            tool_calls: None,
                            tool_call_id: Some(result.tool_use_id.clone()),
                        });
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

        Ok(Self {
            model,
            stream: true,
            messages: out_messages,
            tools: tool_defs,
            max_tokens: Some(max_tokens),
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
        })
    }
}

fn tool_result_text(result: &ToolResult) -> String {
    match &result.content {
        ToolResultContent::Text(text) => text.clone(),
        ToolResultContent::Blocks(blocks) => blocks
            .iter()
            .find_map(|block| match block {
                crate::tools::ToolResultBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default(),
    }
}

#[derive(Debug)]
struct ToolCallState {
    stream_index: usize,
}

/// SSE parser for OpenAI-compatible chat completions.
struct ChatCompletionsSseParser<S> {
    inner: S,
    buffer: Vec<u8>,
    model: String,
    pending: VecDeque<StreamEvent>,
    next_index: usize,
    text_index: Option<usize>,
    saw_tool: bool,
    tool_calls: HashMap<u32, ToolCallState>,
    final_usage: Option<Usage>,
    final_finish_reason: Option<String>,
    emitted_done: bool,
}

impl<S> ChatCompletionsSseParser<S> {
    fn new(stream: S, model: String) -> Self {
        Self {
            inner: stream,
            buffer: Vec::new(),
            model,
            pending: VecDeque::new(),
            next_index: 0,
            text_index: None,
            saw_tool: false,
            tool_calls: HashMap::new(),
            final_usage: None,
            final_finish_reason: None,
            emitted_done: false,
        }
    }

    fn try_next_event(&mut self) -> Option<Result<StreamEvent>> {
        if let Some(event) = self.pending.pop_front() {
            return Some(Ok(event));
        }

        let (pos, delim_len) = find_double_newline(&self.buffer)?;

        let chunk = self.buffer.drain(..pos).collect::<Vec<u8>>();
        self.buffer.drain(..delim_len);

        let chunk_text = String::from_utf8_lossy(&chunk);
        let data = match parse_sse_data(&chunk_text) {
            Ok(value) => value,
            Err(err) => return Some(Err(err)),
        };

        let value = data?;

        if let Err(err) = self.handle_chunk(value) {
            return Some(Err(err));
        }

        self.pending.pop_front().map(Ok)
    }

    fn handle_chunk(&mut self, value: Value) -> Result<()> {
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
            return Ok(());
        }

        if let Some(usage) = value.get("usage") {
            let prompt = usage
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let completion = usage
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            self.final_usage = Some(Usage {
                input_tokens: prompt,
                output_tokens: completion,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            });
        }

        let Some(choices) = value.get("choices").and_then(|v| v.as_array()) else {
            return Ok(());
        };

        let Some(choice) = choices.first() else {
            return Ok(());
        };

        if let Some(finish_reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            self.final_finish_reason = Some(finish_reason.to_string());
        }

        if let Some(delta) = choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                if self.text_index.is_none() {
                    let index = self.next_index;
                    self.next_index += 1;
                    self.text_index = Some(index);
                    self.pending.push_back(StreamEvent::ContentBlockStart {
                        index,
                        block_type: "text".to_string(),
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
                            block_type: "tool_use".to_string(),
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

        if let Some(reason) = self.final_finish_reason.clone()
            && !self.emitted_done
        {
            self.emitted_done = true;

            if let Some(index) = self.text_index.take() {
                self.pending
                    .push_back(StreamEvent::ContentBlockStop { index });
            }

            let tool_indices: Vec<usize> = self
                .tool_calls
                .values()
                .map(|state| state.stream_index)
                .collect();
            for index in tool_indices {
                self.pending
                    .push_back(StreamEvent::ContentBlockStop { index });
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
            self.pending.push_back(StreamEvent::MessageStop);
        }

        Ok(())
    }
}

impl<S, E> Stream for ChatCompletionsSseParser<S>
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
            if let Some(event) = self.try_next_event() {
                return Poll::Ready(Some(event));
            }

            let inner = Pin::new(&mut self.inner);
            match inner.poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    self.buffer.extend_from_slice(&bytes);
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(anyhow!("Stream error: {}", e))));
                }
                Poll::Ready(None) => {
                    let is_empty = self.buffer.iter().all(|b| b.is_ascii_whitespace());
                    if is_empty {
                        return Poll::Ready(None);
                    }
                    if let Some(event) = self.try_next_event() {
                        return Poll::Ready(Some(event));
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

/// Finds the position of a double newline in the buffer.
fn find_double_newline(buffer: &[u8]) -> Option<(usize, usize)> {
    let crlf_pos = buffer.windows(4).position(|w| w == b"\r\n\r\n");
    let lf_pos = buffer.windows(2).position(|w| w == b"\n\n");

    match (crlf_pos, lf_pos) {
        (Some(c), Some(l)) => {
            if l <= c {
                Some((l, 2))
            } else {
                Some((c, 4))
            }
        }
        (Some(c), None) => Some((c, 4)),
        (None, Some(l)) => Some((l, 2)),
        (None, None) => None,
    }
}

fn parse_sse_data(chunk: &str) -> Result<Option<Value>> {
    let mut data_lines = Vec::new();
    for line in chunk.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim());
        }
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let data = data_lines.join("\n");
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return Ok(None);
    }
    let value = serde_json::from_str::<Value>(trimmed)
        .map_err(|err| anyhow!("Failed to parse SSE JSON: {}", err))?;
    Ok(Some(value))
}
