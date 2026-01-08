//! Gemini provider (Google Generative Language API).

use std::collections::{HashMap, HashSet, VecDeque};
use std::pin::Pin;

use anyhow::{Context, Result, anyhow};
use futures_util::Stream;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::providers::{
    ChatContentBlock, ChatMessage, MessageContent, ProviderError, ProviderErrorKind, StreamEvent,
    Usage,
};
use crate::tools::{ToolDefinition, ToolResult, ToolResultContent};

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const SYNTHETIC_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";

/// Gemini API configuration.
#[derive(Debug, Clone)]
pub struct GeminiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_output_tokens: u32,
}

impl GeminiConfig {
    /// Creates a new config from environment.
    ///
    /// Environment variables:
    /// - `GEMINI_API_KEY` (required)
    /// - `GEMINI_BASE_URL` (optional)
    pub fn from_env(
        model: String,
        max_output_tokens: u32,
        config_base_url: Option<&str>,
    ) -> Result<Self> {
        let api_key = std::env::var("GEMINI_API_KEY")
            .context("GEMINI_API_KEY is not set. Set it to use Gemini.")?;
        let base_url = resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_output_tokens,
        })
    }
}

/// Gemini client.
pub struct GeminiClient {
    config: GeminiConfig,
    http: reqwest::Client,
}

impl GeminiClient {
    pub fn new(config: GeminiConfig) -> Self {
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
        let request = build_request(messages, tools, system, self.config.max_output_tokens)?;
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.config.base_url, self.config.model
        );
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
        let event_stream = GeminiSseParser::new(byte_stream, self.config.model.clone());
        Ok(Box::pin(event_stream))
    }
}

fn resolve_base_url(config_base_url: Option<&str>) -> Result<String> {
    if let Ok(env_url) = std::env::var("GEMINI_BASE_URL") {
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
    url::Url::parse(url).with_context(|| format!("Invalid Gemini base URL: {}", url))?;
    Ok(())
}

fn build_headers(api_key: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-goog-api-key",
        HeaderValue::from_str(api_key).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
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

fn build_request(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    system: Option<&str>,
    max_output_tokens: u32,
) -> Result<Value> {
    let active_loop_start = active_loop_start_index(messages);
    let mut contents = Vec::new();
    let mut tool_name_map: HashMap<String, String> = HashMap::new();

    for (idx, msg) in messages.iter().enumerate() {
        let add_thought_signature = idx >= active_loop_start;
        match (&msg.role[..], &msg.content) {
            ("user", MessageContent::Text(text)) => {
                contents.push(json!({
                    "role": "user",
                    "parts": [{"text": text}]
                }));
            }
            ("assistant", MessageContent::Text(text)) => {
                contents.push(json!({
                    "role": "model",
                    "parts": [{"text": text}]
                }));
            }
            ("assistant", MessageContent::Blocks(blocks)) => {
                let mut parts = Vec::new();
                let mut added_signature = false;
                for block in blocks {
                    match block {
                        ChatContentBlock::Text(text) => {
                            parts.push(json!({"text": text}));
                        }
                        ChatContentBlock::ToolUse { id, name, input } => {
                            tool_name_map.insert(id.clone(), name.clone());
                            let mut part = json!({
                                "functionCall": {
                                    "name": name,
                                    "args": input
                                }
                            });
                            if add_thought_signature && !added_signature {
                                part["thoughtSignature"] = json!(SYNTHETIC_THOUGHT_SIGNATURE);
                                added_signature = true;
                            }
                            parts.push(part);
                        }
                        ChatContentBlock::Thinking { .. } => {}
                        ChatContentBlock::ToolResult(_) => {}
                    }
                }

                if !parts.is_empty() {
                    contents.push(json!({
                        "role": "model",
                        "parts": parts
                    }));
                }
            }
            ("user", MessageContent::Blocks(blocks)) => {
                let mut parts = Vec::new();
                let mut tool_results = Vec::new();

                for block in blocks {
                    match block {
                        ChatContentBlock::Text(text) => parts.push(json!({"text": text})),
                        ChatContentBlock::ToolResult(result) => tool_results.push(result),
                        _ => {}
                    }
                }

                for result in tool_results {
                    if let Some(name) = tool_name_map.get(&result.tool_use_id) {
                        parts.push(json!({
                            "functionResponse": {
                                "name": name,
                                "response": {
                                    "content": tool_result_text(result),
                                    "is_error": result.is_error
                                }
                            }
                        }));
                    }
                }

                if !parts.is_empty() {
                    contents.push(json!({
                        "role": "user",
                        "parts": parts
                    }));
                }
            }
            _ => {}
        }
    }

    let tools_value = if tools.is_empty() {
        None
    } else {
        Some(json!([
            {
                "function_declarations": tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema
                        })
                    })
                    .collect::<Vec<_>>()
            }
        ]))
    };

    let mut request = json!({
        "contents": contents,
    });

    if let Some(prompt) = system
        && !prompt.trim().is_empty()
    {
        request["system_instruction"] = json!({
            "parts": [{"text": prompt}]
        });
    }

    if let Some(tools_value) = tools_value {
        request["tools"] = tools_value;
    }

    if max_output_tokens > 0 {
        request["generation_config"] = json!({
            "max_output_tokens": max_output_tokens
        });
    }

    Ok(request)
}

fn active_loop_start_index(messages: &[ChatMessage]) -> usize {
    messages.iter().rposition(matches_user_text).unwrap_or(0)
}

fn matches_user_text(msg: &ChatMessage) -> bool {
    if msg.role != "user" {
        return false;
    }

    match &msg.content {
        MessageContent::Text(text) => !text.trim().is_empty(),
        MessageContent::Blocks(blocks) => blocks.iter().any(|block| match block {
            ChatContentBlock::Text(text) => !text.trim().is_empty(),
            _ => false,
        }),
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
struct GeminiSseParser<S> {
    inner: S,
    buffer: Vec<u8>,
    model: String,
    run_id: String,
    pending: VecDeque<StreamEvent>,
    next_index: usize,
    text_index: Option<usize>,
    last_text: String,
    saw_tool: bool,
    emitted_tool_calls: HashSet<String>,
    final_usage: Option<Usage>,
    final_finish_reason: Option<String>,
    emitted_done: bool,
}

impl<S> GeminiSseParser<S> {
    fn new(stream: S, model: String) -> Self {
        Self {
            inner: stream,
            buffer: Vec::new(),
            model,
            run_id: Uuid::new_v4().to_string(),
            pending: VecDeque::new(),
            next_index: 0,
            text_index: None,
            last_text: String::new(),
            saw_tool: false,
            emitted_tool_calls: HashSet::new(),
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
                .get("status")
                .or_else(|| error.get("code"))
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

        if let Some(usage) = value
            .get("usageMetadata")
            .or_else(|| value.get("usage_metadata"))
        {
            let prompt = usage
                .get("promptTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let completion = usage
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            self.final_usage = Some(Usage {
                input_tokens: prompt,
                output_tokens: completion,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            });
        }

        if let Some(candidates) = value.get("candidates").and_then(|v| v.as_array())
            && let Some(candidate) = candidates.first()
        {
            if let Some(reason) = candidate.get("finishReason").and_then(|v| v.as_str()) {
                self.final_finish_reason = Some(reason.to_string());
            }

            if let Some(content) = candidate.get("content")
                && let Some(parts) = content.get("parts").and_then(|v| v.as_array())
            {
                let mut combined_text = String::new();

                for part in parts {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        combined_text.push_str(text);
                    }
                }

                if !combined_text.is_empty() {
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

                    let delta = if combined_text.starts_with(&self.last_text) {
                        combined_text[self.last_text.len()..].to_string()
                    } else {
                        combined_text.clone()
                    };
                    self.last_text = combined_text;
                    if !delta.is_empty() {
                        self.pending.push_back(StreamEvent::TextDelta {
                            index: self.text_index.unwrap_or(0),
                            text: delta,
                        });
                    }
                }

                for part in parts {
                    if let Some(call) = part.get("functionCall") {
                        let name = call.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let args = call.get("args").unwrap_or(&Value::Null);
                        let key = format!("{}:{}", name, args);
                        if self.emitted_tool_calls.contains(&key) {
                            continue;
                        }
                        self.emitted_tool_calls.insert(key);

                        let tool_id = format!("gemini-{}-{}", self.run_id, self.next_index);
                        let index = self.next_index;
                        self.next_index += 1;
                        self.saw_tool = true;

                        self.pending.push_back(StreamEvent::ContentBlockStart {
                            index,
                            block_type: "tool_use".to_string(),
                            id: Some(tool_id.clone()),
                            name: Some(name.to_string()),
                        });

                        let args_json = if args.is_null() {
                            "{}".to_string()
                        } else {
                            serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string())
                        };
                        self.pending.push_back(StreamEvent::InputJsonDelta {
                            index,
                            partial_json: args_json,
                        });
                        self.pending
                            .push_back(StreamEvent::ContentBlockStop { index });
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

            let usage = self.final_usage.clone().unwrap_or_default();
            let stop_reason = if self.saw_tool {
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

impl<S, E> Stream for GeminiSseParser<S>
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
        "MAX_TOKENS" | "max_tokens" => "max_tokens".to_string(),
        "STOP" | "stop" => "stop".to_string(),
        other => other.to_lowercase(),
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
