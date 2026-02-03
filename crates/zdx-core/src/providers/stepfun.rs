//! StepFun provider (Step-3.5-Flash) with text-based tool call parsing.
//!
//! StepFun models output tool calls as XML-like text in the content field.
//! This provider wraps the OpenAI-compatible client and transforms those
//! text-based tool calls into proper ToolUse stream events.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::{Context as _, Result};
use futures_util::Stream;
use reqwest::header::HeaderMap;
use uuid::Uuid;

use crate::prompts::STEPFUN_AGENTIC_PROMPT_TEMPLATE;
use crate::providers::openai::chat_completions::{
    OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig,
};
use crate::providers::text_tool_parser::{has_complete_tool_call, parse_tool_calls};
use crate::providers::thinking_parser::{parse_thinking, strip_think_start};
use crate::providers::{
    ChatContentBlock, ChatMessage, ContentBlockType, MessageContent, ProviderResult,
    ProviderStream, StreamEvent, Usage,
};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://api.stepfun.ai/v1";

/// StepFun API configuration.
#[derive(Debug, Clone)]
pub struct StepfunConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_enabled: bool,
}

impl StepfunConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `STEPFUN_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `STEPFUN_API_KEY` (fallback if not in config)
    /// - `STEPFUN_BASE_URL` (optional)
    pub fn from_env(
        model: String,
        max_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        prompt_cache_key: Option<String>,
        thinking_enabled: bool,
    ) -> Result<Self> {
        let api_key = resolve_api_key(config_api_key)?;
        let base_url = resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
            prompt_cache_key,
            thinking_enabled,
        })
    }
}

/// StepFun client with text-based tool call parsing.
pub struct StepfunClient {
    inner: OpenAIChatCompletionsClient,
}

impl StepfunClient {
    pub fn new(config: StepfunConfig) -> Self {
        Self {
            inner: OpenAIChatCompletionsClient::new(OpenAIChatCompletionsConfig {
                api_key: config.api_key,
                base_url: config.base_url,
                model: config.model,
                max_tokens: config.max_tokens,
                max_completion_tokens: None,
                reasoning_effort: None,
                prompt_cache_key: config.prompt_cache_key,
                extra_headers: HeaderMap::new(),
                include_usage: true,
                include_reasoning_content: config.thinking_enabled,
                thinking: Some(config.thinking_enabled.into()),
            }),
        }
    }

    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        // Convert ToolUse blocks to XML text format for StepFun
        // (StepFun doesn't support native tool_calls in assistant messages)
        let messages = convert_tool_uses_to_text(messages);

        let system = merge_stepfun_system_prompt(system);
        let inner_stream = self
            .inner
            .send_messages_stream(&messages, tools, system.as_deref())
            .await?;

        // Wrap with tool call parser if tools are provided
        if tools.is_empty() {
            Ok(inner_stream)
        } else {
            Ok(Box::pin(StepfunToolCallTransformer::new(inner_stream)))
        }
    }
}

/// Stream transformer that converts text-based tool calls to ToolUse events
/// and handles reasoning-to-content transitions at `</think>` boundaries.
struct StepfunToolCallTransformer {
    inner: ProviderStream,
    /// Accumulated text content that may contain tool calls
    text_buffer: String,
    /// Current text block index (if we started one)
    text_index: Option<usize>,
    /// Upstream text block index (for tracking completions to ignore)
    upstream_text_index: Option<usize>,
    /// Current reasoning block index (if we started one)
    reasoning_index: Option<usize>,
    /// Upstream reasoning block index (for tracking completions to ignore)
    upstream_reasoning_index: Option<usize>,
    /// Whether reasoning is complete (saw `</think>`)
    reasoning_complete: bool,
    /// Whether we've emitted any tool calls
    emitted_tool_calls: bool,
    /// Next available block index
    next_index: usize,
    /// Pending events to emit
    pending: VecDeque<ProviderResult<StreamEvent>>,
    /// Final usage to emit
    final_usage: Option<Usage>,
    /// Final stop reason to emit if no tool calls were used
    final_stop_reason: Option<String>,
    /// Whether to trim leading whitespace on the next text delta
    trim_leading_text_once: bool,
    /// Whether stream has ended
    ended: bool,
}

impl StepfunToolCallTransformer {
    fn new(inner: ProviderStream) -> Self {
        Self {
            inner,
            text_buffer: String::new(),
            text_index: None,
            upstream_text_index: None,
            upstream_reasoning_index: None,
            reasoning_index: None,
            reasoning_complete: false,
            emitted_tool_calls: false,
            next_index: 0,
            pending: VecDeque::new(),
            final_usage: None,
            final_stop_reason: None,
            trim_leading_text_once: false,
            ended: false,
        }
    }

    /// Find the position of the first tool call marker in content.
    /// Uses lenient matching for tool_call while keeping function detection strict.
    fn first_tool_marker(content: &str) -> Option<usize> {
        // Use prefix matching (without closing > or =) to catch whitespace variants
        let tool_call_pos = content.find("<tool_call");
        let function_pos = content.find("<function=");

        match (tool_call_pos, function_pos) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// Ensure a content block is started, returning the block index.
    fn ensure_block(&mut self, slot: &mut Option<usize>, block_type: ContentBlockType) -> usize {
        if let Some(idx) = *slot {
            idx
        } else {
            let index = self.next_index;
            self.next_index += 1;
            *slot = Some(index);
            self.pending.push_back(Ok(StreamEvent::ContentBlockStart {
                index,
                block_type,
                id: None,
                name: None,
            }));
            index
        }
    }

    /// Ensure a reasoning block is started, returning the block index.
    fn ensure_reasoning_block(&mut self) -> usize {
        // Work around borrow checker by using a temporary
        let mut slot = self.reasoning_index;
        let index = self.ensure_block(&mut slot, ContentBlockType::Reasoning);
        self.reasoning_index = slot;
        index
    }

    /// Ensure a text block is started, returning the block index.
    fn ensure_text_block(&mut self) -> usize {
        // Work around borrow checker by using a temporary
        let mut slot = self.text_index;
        let index = self.ensure_block(&mut slot, ContentBlockType::Text);
        self.text_index = slot;
        index
    }

    /// Append text to buffer and process for tool calls or flush if safe.
    fn push_text_and_process(&mut self, chunk: &str) {
        self.text_buffer.push_str(chunk);
        if has_complete_tool_call(&self.text_buffer) {
            self.process_buffer();
        } else {
            self.flush_remaining_text(false);
        }
    }

    /// Process accumulated text buffer for tool calls.
    fn process_buffer(&mut self) {
        if self.text_buffer.is_empty() {
            return;
        }

        // Check if we have complete tool calls
        if !has_complete_tool_call(&self.text_buffer) {
            return;
        }

        // Find the first tool call marker
        let first_marker = Self::first_tool_marker(&self.text_buffer);

        // Emit any text before the first tool call BEFORE emitting tool calls
        if let Some(pos) = first_marker
            && pos > 0
        {
            // Use split_off to avoid double allocation
            let tail = self.text_buffer.split_off(pos);
            let text_before = std::mem::replace(&mut self.text_buffer, tail);
            // Only emit if there's non-whitespace content
            if !text_before.trim().is_empty() {
                self.emit_text(&text_before);
            }
        }

        // Now parse tool calls from buffer (starts with tool call marker)
        let (tool_calls, remaining) = parse_tool_calls(&self.text_buffer);

        // Handle parse failure - emit as text to avoid stream stall
        if tool_calls.is_empty() {
            // Parsing failed despite has_complete_tool_call being true
            // This can happen with malformed tags - emit as text to avoid stall
            self.flush_remaining_text(true);
            return;
        }

        self.emitted_tool_calls = true;

        // Close text block before emitting tool calls
        if let Some(text_idx) = self.text_index.take() {
            self.pending
                .push_back(Ok(StreamEvent::ContentBlockCompleted { index: text_idx }));
        }

        // Emit tool call events
        for tool_call in tool_calls {
            let tool_id = format!("toolcall-{}", Uuid::new_v4());
            let index = self.next_index;
            self.next_index += 1;

            // Start tool use block
            self.pending.push_back(Ok(StreamEvent::ContentBlockStart {
                index,
                block_type: ContentBlockType::ToolUse,
                id: Some(tool_id),
                name: Some(tool_call.name),
            }));

            // Emit arguments as JSON (use "{}" as fallback for serialization errors)
            let args_json =
                serde_json::to_string(&tool_call.arguments).unwrap_or_else(|_| "{}".to_string());
            self.pending.push_back(Ok(StreamEvent::InputJsonDelta {
                index,
                partial_json: args_json,
            }));

            // Complete tool use block
            self.pending
                .push_back(Ok(StreamEvent::ContentBlockCompleted { index }));
        }

        // Update buffer with remaining text (could include text after tool calls)
        self.text_buffer = remaining;
    }

    /// Flush remaining text buffer.
    ///
    /// If `force` is true, emit everything (used at stream end for incomplete tool calls).
    fn flush_remaining_text(&mut self, force: bool) {
        if self.text_buffer.is_empty() {
            return;
        }

        if force {
            // Emit everything at stream end, preserving content
            // Only skip if the entire buffer is whitespace
            let text = std::mem::take(&mut self.text_buffer);
            if !text.trim().is_empty() {
                self.emit_text(&text);
            }
            return;
        }

        // Check for tool call markers
        let first_marker = Self::first_tool_marker(&self.text_buffer);

        match first_marker {
            Some(0) => {
                // Tool call at start - don't emit, wait for completion
            }
            Some(pos) => {
                // Text before tool call - emit text, keep tool call part
                // Use split_off to avoid double allocation
                let tail = self.text_buffer.split_off(pos);
                let text = std::mem::replace(&mut self.text_buffer, tail);
                // Only emit if there's non-whitespace content
                if !text.trim().is_empty() {
                    self.emit_text(&text);
                }
            }
            None => {
                // No tool call markers - check for partial at end
                if !ends_with_partial_tool_marker(&self.text_buffer) {
                    // Safe to emit - no markers or partials
                    let text = std::mem::take(&mut self.text_buffer);
                    if !text.trim().is_empty() {
                        self.emit_text(&text);
                    }
                }
            }
        }
    }

    /// Emit text content, starting a text block if needed.
    /// Trims leading whitespace once after reasoning completes to avoid extra blank lines.
    fn emit_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut text = text;

        // Trim leading whitespace once after reasoning completes
        if self.trim_leading_text_once {
            let trimmed = text.trim_start();
            if trimmed.is_empty() {
                // Keep trim flag for next delta if this was only whitespace
                return;
            }
            text = trimmed;
            self.trim_leading_text_once = false;
        }

        if text.is_empty() {
            return;
        }

        let index = self.ensure_text_block();

        self.pending.push_back(Ok(StreamEvent::TextDelta {
            index,
            text: text.to_string(),
        }));
    }

    /// Process reasoning content that may contain `</think>` tag.
    /// Handles the case where content bleeds into reasoning after the tag.
    fn process_reasoning(&mut self, reasoning: &str) {
        // If reasoning is already complete, route subsequent deltas to text buffer
        // (StepFun may continue sending reasoning deltas after </think>)
        if self.reasoning_complete {
            self.push_text_and_process(reasoning);
            return;
        }

        // Strip <think> opening tag if present at the start
        let reasoning = strip_think_start(reasoning);

        // Check if THIS delta contains </think>
        let result = parse_thinking(reasoning);

        if result.thinking_complete {
            self.reasoning_complete = true;

            // Emit reasoning portion (before </think>) from THIS delta only
            if !result.reasoning.is_empty() {
                let index = self.ensure_reasoning_block();
                self.pending.push_back(Ok(StreamEvent::ReasoningDelta {
                    index,
                    reasoning: result.reasoning,
                }));
            }

            // Close reasoning block
            if let Some(reasoning_idx) = self.reasoning_index.take() {
                self.pending
                    .push_back(Ok(StreamEvent::ContentBlockCompleted {
                        index: reasoning_idx,
                    }));
            }

            // Trim leading whitespace on the first text delta after reasoning
            self.trim_leading_text_once = true;

            // Route content that bled into reasoning to the text buffer
            // This ensures proper tool call detection instead of leaking raw tags
            if let Some(content) = result.content {
                self.push_text_and_process(&content);
            }
        } else {
            // No </think> in this delta, emit as-is
            let index = self.ensure_reasoning_block();
            self.pending.push_back(Ok(StreamEvent::ReasoningDelta {
                index,
                reasoning: reasoning.to_string(),
            }));
        }
    }

    /// Emit completion events.
    fn emit_completion(&mut self) {
        // Close any open reasoning block
        if let Some(reasoning_idx) = self.reasoning_index.take() {
            self.pending
                .push_back(Ok(StreamEvent::ContentBlockCompleted {
                    index: reasoning_idx,
                }));
        }

        // Close any open text block
        if let Some(text_idx) = self.text_index.take() {
            self.pending
                .push_back(Ok(StreamEvent::ContentBlockCompleted { index: text_idx }));
        }

        let stop_reason = if self.emitted_tool_calls {
            Some("tool_use".to_string())
        } else {
            self.final_stop_reason.clone()
        };

        // Emit message delta with stop reason
        self.pending.push_back(Ok(StreamEvent::MessageDelta {
            stop_reason,
            usage: self.final_usage.clone(),
        }));

        self.pending.push_back(Ok(StreamEvent::MessageCompleted));
    }

    fn handle_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::MessageStart { model, usage } => {
                self.pending
                    .push_back(Ok(StreamEvent::MessageStart { model, usage }));
            }
            StreamEvent::ContentBlockStart {
                index,
                block_type: ContentBlockType::Text,
                ..
            } => {
                // Track upstream's text block index for completion handling
                self.upstream_text_index = Some(index);
                // Only use upstream's index if we haven't started our own text block
                // (e.g., from content bleeding after </think>)
                if self.text_index.is_none() {
                    self.text_index = Some(index);
                    self.pending.push_back(Ok(StreamEvent::ContentBlockStart {
                        index,
                        block_type: ContentBlockType::Text,
                        id: None,
                        name: None,
                    }));
                }
                self.next_index = self.next_index.max(index + 1);
            }
            StreamEvent::ContentBlockStart {
                index,
                block_type: ContentBlockType::Reasoning,
                ..
            } => {
                // Don't emit reasoning block starts - we'll manage our own via process_reasoning
                // Just track the index for completion handling
                self.upstream_reasoning_index = Some(index);
                self.next_index = self.next_index.max(index + 1);
            }
            StreamEvent::TextDelta { text, .. } => {
                // Accumulate text and process
                self.push_text_and_process(&text);
            }
            StreamEvent::ReasoningDelta { reasoning, .. } => {
                // Process reasoning through the thinking parser
                self.process_reasoning(&reasoning);
            }
            StreamEvent::ContentBlockCompleted { index } => {
                // Handle text block completion from upstream
                if self.upstream_text_index == Some(index) {
                    // Process any remaining buffer
                    self.process_buffer();
                    self.flush_remaining_text(false);

                    // Complete our text block (which might have a different index if we started it ourselves)
                    if let Some(text_idx) = self.text_index.take() {
                        self.pending
                            .push_back(Ok(StreamEvent::ContentBlockCompleted { index: text_idx }));
                    }
                    self.upstream_text_index = None;
                } else if self.upstream_reasoning_index == Some(index) {
                    // Upstream reasoning completion - ignore (we emit our own reasoning blocks)
                    self.upstream_reasoning_index = None;
                } else if self.reasoning_index == Some(index) {
                    // Reasoning block completed by upstream - we handle our own completion
                    // in process_reasoning when we see </think>, so ignore here
                } else if self.text_index != Some(index) {
                    // Pass through completion for other blocks
                    self.pending
                        .push_back(Ok(StreamEvent::ContentBlockCompleted { index }));
                }
            }
            StreamEvent::MessageDelta { stop_reason, usage } => {
                // Store stop reason + usage for later
                if stop_reason.is_some() {
                    self.final_stop_reason = stop_reason;
                }
                if usage.is_some() {
                    self.final_usage = usage;
                }
            }
            StreamEvent::MessageCompleted => {
                // Process any remaining buffer before completing
                self.process_buffer();
                self.flush_remaining_text(true);
                self.emit_completion();
            }
            // Pass through other events
            other => {
                self.pending.push_back(Ok(other));
            }
        }
    }
}

impl Stream for StepfunToolCallTransformer {
    type Item = ProviderResult<StreamEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            // Return pending events first
            if let Some(event) = self.pending.pop_front() {
                return Poll::Ready(Some(event));
            }

            if self.ended {
                return Poll::Ready(None);
            }

            // Poll inner stream
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => {
                    self.handle_event(event);
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(e)));
                }
                Poll::Ready(None) => {
                    self.ended = true;
                    // Process any remaining content
                    self.process_buffer();
                    self.flush_remaining_text(true);
                    if self.text_index.is_some() || !self.pending.is_empty() {
                        self.emit_completion();
                    }
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Check if content ends with a potential partial tool call marker.
///
/// This prevents flushing text that could become a tool call tag
/// with more data (e.g., "<tool_" could become "<tool_call>").
/// Also handles whitespace variants like "<tool_call " or "<function ".
fn ends_with_partial_tool_marker(content: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "<t",
        "<to",
        "<too",
        "<tool",
        "<tool_",
        "<tool_c",
        "<tool_ca",
        "<tool_cal",
        "<tool_call",
        "<tool_call ",
        "<tool_call\t",
        "<tool_call\n",
        "<f",
        "<fu",
        "<fun",
        "<func",
        "<funct",
        "<functi",
        "<functio",
        "<function",
        "<function ",
        "<function\t",
        "<function\n",
        "<function=",
    ];

    // Check for lone < at end
    if content.ends_with('<') {
        return true;
    }

    for prefix in PREFIXES {
        if content.ends_with(prefix) {
            return true;
        }
    }
    false
}

/// Merges the StepFun base prompt with the provided system prompt.
///
/// Always includes the StepFun template first, appending any caller-provided system prompt.
fn merge_stepfun_system_prompt(system: Option<&str>) -> Option<String> {
    let base = STEPFUN_AGENTIC_PROMPT_TEMPLATE.trim();
    let merged = match system {
        Some(prompt) if !prompt.trim().is_empty() => {
            format!("{}\n\n{}", base, prompt.trim())
        }
        _ => base.to_string(),
    };
    Some(merged)
}

/// Converts ToolUse blocks in assistant messages to XML text format.
///
/// StepFun doesn't support native OpenAI tool_calls in assistant messages,
/// so we need to serialize tool calls back to the XML text format that StepFun expects.
fn convert_tool_uses_to_text(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|msg| {
            if msg.role != "assistant" {
                return msg.clone();
            }

            match &msg.content {
                MessageContent::Blocks(blocks) => {
                    let mut new_blocks = Vec::new();
                    let mut tool_call_text = String::new();

                    for block in blocks {
                        match block {
                            ChatContentBlock::ToolUse { name, input, .. } => {
                                // Convert to XML text format
                                tool_call_text.push_str("<tool_call>\n");
                                tool_call_text.push_str(&format!("<function={}>\n", name));
                                if let Some(obj) = input.as_object() {
                                    for (key, value) in obj {
                                        let value_str = match value {
                                            serde_json::Value::String(s) => s.clone(),
                                            other => other.to_string(),
                                        };
                                        tool_call_text.push_str(&format!(
                                            "<parameter={}>\n{}\n</parameter>\n",
                                            key, value_str
                                        ));
                                    }
                                }
                                tool_call_text.push_str("</function>\n");
                                tool_call_text.push_str("</tool_call>");
                            }
                            other => {
                                // Flush any accumulated tool call text before this block
                                if !tool_call_text.is_empty() {
                                    new_blocks.push(ChatContentBlock::Text(std::mem::take(
                                        &mut tool_call_text,
                                    )));
                                }
                                new_blocks.push(other.clone());
                            }
                        }
                    }

                    // Flush any remaining tool call text
                    if !tool_call_text.is_empty() {
                        new_blocks.push(ChatContentBlock::Text(tool_call_text));
                    }

                    ChatMessage {
                        role: msg.role.clone(),
                        content: MessageContent::Blocks(new_blocks),
                    }
                }
                _ => msg.clone(),
            }
        })
        .collect()
}

fn resolve_base_url(config_base_url: Option<&str>) -> Result<String> {
    if let Ok(env_url) = std::env::var("STEPFUN_BASE_URL") {
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
    url::Url::parse(url).with_context(|| format!("Invalid StepFun base URL: {}", url))?;
    Ok(())
}

/// Resolves API key with precedence: config > env.
fn resolve_api_key(config_api_key: Option<&str>) -> Result<String> {
    // Try config value first
    if let Some(key) = config_api_key {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    // Fall back to env var
    std::env::var("STEPFUN_API_KEY")
        .context("No API key available. Set STEPFUN_API_KEY or api_key in [providers.stepfun].")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_convert_tool_uses_to_text_basic() {
        let messages = vec![ChatMessage {
            role: "assistant".to_string(),
            content: MessageContent::Blocks(vec![
                ChatContentBlock::Text("Let me run that for you.".to_string()),
                ChatContentBlock::ToolUse {
                    id: "call_123".to_string(),
                    name: "bash".to_string(),
                    input: json!({"command": "date"}),
                },
            ]),
        }];

        let converted = convert_tool_uses_to_text(&messages);
        assert_eq!(converted.len(), 1);

        if let MessageContent::Blocks(blocks) = &converted[0].content {
            assert_eq!(blocks.len(), 2);
            // First block should be text
            assert!(
                matches!(&blocks[0], ChatContentBlock::Text(t) if t == "Let me run that for you.")
            );
            // Second block should be tool call as text
            if let ChatContentBlock::Text(tool_text) = &blocks[1] {
                assert!(tool_text.contains("<tool_call>"));
                assert!(tool_text.contains("<function=bash>"));
                assert!(tool_text.contains("<parameter=command>"));
                assert!(tool_text.contains("date"));
                assert!(tool_text.contains("</tool_call>"));
            } else {
                panic!("Expected text block for tool call");
            }
        } else {
            panic!("Expected Blocks content");
        }
    }

    #[test]
    fn test_convert_tool_uses_preserves_user_messages() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Text("Hello".to_string()),
        }];

        let converted = convert_tool_uses_to_text(&messages);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "user");
        assert!(matches!(&converted[0].content, MessageContent::Text(t) if t == "Hello"));
    }

    #[test]
    fn test_convert_tool_uses_multiple_params() {
        let messages = vec![ChatMessage {
            role: "assistant".to_string(),
            content: MessageContent::Blocks(vec![ChatContentBlock::ToolUse {
                id: "call_456".to_string(),
                name: "write".to_string(),
                input: json!({"path": "test.txt", "content": "hello world"}),
            }]),
        }];

        let converted = convert_tool_uses_to_text(&messages);

        if let MessageContent::Blocks(blocks) = &converted[0].content {
            if let ChatContentBlock::Text(tool_text) = &blocks[0] {
                assert!(tool_text.contains("<function=write>"));
                assert!(tool_text.contains("<parameter=path>"));
                assert!(tool_text.contains("test.txt"));
                assert!(tool_text.contains("<parameter=content>"));
                assert!(tool_text.contains("hello world"));
            } else {
                panic!("Expected text block");
            }
        } else {
            panic!("Expected Blocks content");
        }
    }

    #[test]
    fn test_ends_with_partial_tool_marker() {
        // Partial markers should return true
        assert!(ends_with_partial_tool_marker("Hello <"));
        assert!(ends_with_partial_tool_marker("Hello <t"));
        assert!(ends_with_partial_tool_marker("Hello <tool"));
        assert!(ends_with_partial_tool_marker("Hello <tool_"));
        assert!(ends_with_partial_tool_marker("Hello <tool_call"));
        assert!(ends_with_partial_tool_marker("Hello <f"));
        assert!(ends_with_partial_tool_marker("Hello <function"));
        assert!(ends_with_partial_tool_marker("Hello <function="));

        // Complete markers should return false (they're not partial)
        assert!(!ends_with_partial_tool_marker("Hello <tool_call>"));
        assert!(!ends_with_partial_tool_marker("Hello <function=test>"));

        // No markers should return false
        assert!(!ends_with_partial_tool_marker("Hello"));
        assert!(!ends_with_partial_tool_marker("Hello world"));
        assert!(!ends_with_partial_tool_marker(""));
    }
}
