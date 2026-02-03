//! Agent module for UI-agnostic execution.
//!
//! The agent drives the provider + tool loop and emits `AgentEvent`s
//! via async channels. No direct stdout/stderr writes occur in this module.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, timeout};

use crate::config::Config;
use crate::core::events::{AgentEvent, ErrorKind, ToolOutput};
use crate::core::interrupt::{self, InterruptedError};
use crate::providers::anthropic::{
    AnthropicClient, AnthropicConfig, ClaudeCliClient, ClaudeCliConfig,
};
use crate::providers::gemini::{
    GeminiCliClient, GeminiCliConfig, GeminiClient, GeminiConfig, GeminiThinkingConfig,
};
use crate::providers::mimo::{MimoClient, MimoConfig};
use crate::providers::moonshot::{MoonshotClient, MoonshotConfig};
use crate::providers::openai::{OpenAIClient, OpenAICodexClient, OpenAICodexConfig, OpenAIConfig};
use crate::providers::openrouter::{OpenRouterClient, OpenRouterConfig};
use crate::providers::{
    ChatContentBlock, ChatMessage, ContentBlockType, ProviderError, ProviderKind, ProviderStream,
    ReasoningBlock, ReplayToken, StreamEvent, resolve_provider,
};
use crate::tools::{ToolContext, ToolDefinition, ToolRegistry, ToolResult, ToolSet};

/// Options for agent execution.
#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// Root directory for file operations.
    pub root: PathBuf,
    /// Tool configuration (registry + selection).
    pub tool_config: ToolConfig,
}

/// Tool configuration for agent execution.
#[derive(Debug, Clone)]
pub struct ToolConfig {
    pub registry: ToolRegistry,
    pub selection: ToolSelection,
}

impl ToolConfig {
    pub fn new(registry: ToolRegistry, selection: ToolSelection) -> Self {
        Self {
            registry,
            selection,
        }
    }

    pub fn with_selection(mut self, selection: ToolSelection) -> Self {
        self.selection = selection;
        self
    }
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            registry: ToolRegistry::builtins(),
            selection: ToolSelection::default(),
        }
    }
}

/// Tool selection strategy for agent execution.
#[derive(Debug, Clone)]
pub enum ToolSelection {
    /// Use provider-configured tools if set; otherwise fall back to a tool set.
    Auto { base: ToolSet, include: Vec<String> },
    /// Use a named tool set (with optional includes).
    ToolSet { base: ToolSet, include: Vec<String> },
    /// Use an explicit list of tools (full override).
    Explicit(Vec<String>),
    /// Use all tools in the registry.
    All,
}

impl Default for ToolSelection {
    fn default() -> Self {
        ToolSelection::Auto {
            base: ToolSet::Default,
            include: Vec::new(),
        }
    }
}

/// Channel-based event sender (async, bounded).
///
/// Used with `run_turn` for concurrent rendering and thread persistence.
/// Events are wrapped in `Arc` for efficient cloning to multiple consumers.
pub type AgentEventTx = mpsc::Sender<Arc<AgentEvent>>;

/// Channel-based event receiver (async, bounded).
pub type AgentEventRx = mpsc::Receiver<Arc<AgentEvent>>;

/// Default channel capacity for event streams.
///
/// Set higher (128) to accommodate best-effort delta sends without blocking.
pub const DEFAULT_EVENT_CHANNEL_CAPACITY: usize = 128;

/// Creates a bounded event channel with the default capacity.
pub fn create_event_channel() -> (AgentEventTx, AgentEventRx) {
    mpsc::channel(DEFAULT_EVENT_CHANNEL_CAPACITY)
}

/// Event sender wrapper that provides best-effort and reliable send modes.
///
/// Use `send_delta()` for high-volume events (TextDelta) that can be dropped
/// if the consumer is slow. Use `send_important()` for events that must be
/// delivered (ToolStarted, ToolCompleted, Completed, Error, Interrupted).
#[derive(Clone)]
pub struct EventSender {
    tx: AgentEventTx,
}

impl EventSender {
    /// Creates a new EventSender wrapping the given channel sender.
    pub fn new(tx: AgentEventTx) -> Self {
        Self { tx }
    }

    /// Best-effort send: never awaits, drops if channel is full.
    /// Use for high-volume events like TextDelta that can afford loss.
    pub fn send_delta(&self, ev: AgentEvent) {
        let _ = self.tx.try_send(Arc::new(ev));
    }

    /// Reliable send: awaits delivery.
    /// Use for important events (tool lifecycle, final, errors).
    pub async fn send_important(&self, ev: AgentEvent) {
        let _ = self.tx.send(Arc::new(ev)).await;
    }
}

enum ProviderClient {
    Anthropic(AnthropicClient),
    ClaudeCli(ClaudeCliClient),
    OpenAICodex(OpenAICodexClient),
    OpenAI(OpenAIClient),
    OpenRouter(OpenRouterClient),
    Mimo(MimoClient),
    Moonshot(MoonshotClient),
    Gemini(GeminiClient),
    GeminiCli(GeminiCliClient),
}

impl ProviderClient {
    async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[crate::tools::ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        match self {
            ProviderClient::Anthropic(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::ClaudeCli(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::OpenAICodex(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::OpenAI(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::OpenRouter(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::Mimo(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::Moonshot(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::Gemini(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::GeminiCli(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
        }
    }
}

/// Spawns a broadcast task that distributes events to multiple consumers.
///
/// Uses `try_send` (best-effort) to prevent slow consumers from blocking
/// others. Events are dropped if a consumer's channel is full. Closed
/// channels are automatically removed.
///
/// The task exits when the source channel closes.
///
/// # Example
///
/// ```ignore
/// let (agent_tx, agent_rx) = create_event_channel();
/// let (render_tx, render_rx) = create_event_channel();
/// let (persist_tx, persist_rx) = create_event_channel();
///
/// let broadcaster = spawn_broadcaster(agent_rx, vec![render_tx, persist_tx]);
///
/// // Agent sends to agent_tx
/// // Renderer receives from render_rx
/// // Persister receives from persist_rx
/// ```
pub fn spawn_broadcaster(
    mut rx: AgentEventRx,
    mut subscribers: Vec<AgentEventTx>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            subscribers.retain(|tx| {
                match tx.try_send(event.clone()) {
                    Ok(()) => true,
                    Err(TrySendError::Full(_)) => true, // drop this event, keep channel
                    Err(TrySendError::Closed(_)) => false, // remove closed channel
                }
            });
        }
    })
}

/// Builder for accumulating tool use data from streaming events.
#[derive(Debug, Clone)]
pub struct ToolUseBuilder {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub input_json: String,
    pub input_preview_len: usize,
}

/// Builder for accumulating thinking block data from streaming events.
#[derive(Debug, Clone)]
pub struct ThinkingBuilder {
    pub index: usize,
    pub text: String,
    pub signature: String,
    pub replay: Option<ReplayToken>,
    pub had_delta: bool,
}

/// Finalized tool use with parsed input (ready for execution).
#[derive(Debug, Clone)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// Builder for accumulating all assistant turn content from streaming events.
/// Consolidates thinking, reasoning, text, and tool use into a single struct.
#[derive(Debug, Default)]
pub struct AssistantTurnBuilder {
    pub thinking_blocks: Vec<ThinkingBuilder>,
    pub text: String,
    pub tool_uses: Vec<ToolUseBuilder>,
}

impl AssistantTurnBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Converts accumulated content into ChatContentBlocks for API messages.
    pub fn into_blocks(self, finalized_tools: Vec<ToolUse>) -> Vec<ChatContentBlock> {
        let mut blocks = Vec::with_capacity(self.thinking_blocks.len() + 1 + finalized_tools.len());

        // Add reasoning blocks first (order matters for API)
        for tb in self.thinking_blocks {
            let text = if tb.text.is_empty() {
                None
            } else {
                Some(tb.text)
            };
            blocks.push(ChatContentBlock::Reasoning(ReasoningBlock {
                text,
                replay: tb.replay,
            }));
        }

        // Add text block if any
        if !self.text.is_empty() {
            blocks.push(ChatContentBlock::Text(self.text));
        }

        // Add tool_use blocks
        for tu in finalized_tools {
            blocks.push(ChatContentBlock::ToolUse {
                id: tu.id,
                name: tu.name,
                input: tu.input,
            });
        }

        blocks
    }

    /// Finds a tool use builder by index.
    pub fn find_tool_use_mut(&mut self, index: usize) -> Option<&mut ToolUseBuilder> {
        self.tool_uses.iter_mut().find(|t| t.index == index)
    }

    /// Finds a thinking block by index.
    pub fn find_thinking_mut(&mut self, index: usize) -> Option<&mut ThinkingBuilder> {
        self.thinking_blocks.iter_mut().find(|t| t.index == index)
    }
}

impl ToolUseBuilder {
    /// Finalizes the builder by parsing the accumulated JSON input.
    /// Returns an error if the JSON is malformed.
    pub fn finalize(self) -> Result<ToolUse, serde_json::Error> {
        let input = serde_json::from_str(&self.input_json)?;
        Ok(ToolUse {
            id: self.id,
            name: self.name,
            input,
        })
    }
}

fn extract_partial_tool_input(name: &str, input_json: &str) -> Option<String> {
    let field = match name {
        "write" => "content",
        "edit" => "new",
        _ => return None,
    };

    extract_partial_json_string_field(input_json, field)
}

fn extract_partial_json_string_field(input_json: &str, field: &str) -> Option<String> {
    let key = format!("\"{}\"", field);
    let mut search_start = 0;

    while let Some(rel_pos) = input_json[search_start..].find(&key) {
        let key_pos = search_start + rel_pos;
        let mut idx = key_pos + key.len();
        let bytes = input_json.as_bytes();

        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if bytes.get(idx) != Some(&b':') {
            search_start = key_pos + key.len();
            continue;
        }
        idx += 1;

        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if bytes.get(idx) != Some(&b'"') {
            search_start = key_pos + key.len();
            continue;
        }
        idx += 1;

        return Some(decode_partial_json_string(&input_json[idx..]));
    }

    None
}

fn decode_partial_json_string(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => break,
            '\\' => {
                if let Some(esc) = chars.next() {
                    match esc {
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'b' => out.push('\u{0008}'),
                        'f' => out.push('\u{000c}'),
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        'u' => {
                            let mut hex = String::new();
                            for _ in 0..4 {
                                if let Some(h) = chars.next() {
                                    hex.push(h);
                                } else {
                                    return out;
                                }
                            }
                            if let Ok(code) = u32::from_str_radix(&hex, 16)
                                && let Some(decoded) = char::from_u32(code)
                            {
                                out.push(decoded);
                            }
                        }
                        other => out.push(other),
                    }
                }
            }
            other => out.push(other),
        }
    }

    out
}

/// Sends an error event via the async channel and returns the original error.
/// This preserves the full error chain (including ProviderError details) for callers.
async fn emit_error_async(err: anyhow::Error, sender: &EventSender) -> anyhow::Error {
    let event = if let Some(provider_err) = err.downcast_ref::<ProviderError>() {
        AgentEvent::Error {
            kind: provider_err.kind.clone().into(),
            message: provider_err.message.clone(),
            details: provider_err.details.clone(),
        }
    } else {
        AgentEvent::Error {
            kind: ErrorKind::Internal,
            message: err.to_string(),
            details: None,
        }
    };
    sender.send_important(event).await;
    err
}

fn merge_tool_defs(
    mut base: Vec<ToolDefinition>,
    include: &[String],
    registry: &ToolRegistry,
) -> Vec<ToolDefinition> {
    if include.is_empty() {
        return base;
    }

    let extra = registry.tools_from_names(include.iter().map(String::as_str));
    if extra.is_empty() {
        return base;
    }

    let mut seen: HashSet<String> = base.iter().map(|t| t.name.to_lowercase()).collect();
    for tool in extra {
        if seen.insert(tool.name.to_lowercase()) {
            base.push(tool);
        }
    }
    base
}

/// Timeout for stream polling to allow interrupt checks.
const STREAM_POLL_TIMEOUT: Duration = Duration::from_millis(250);

/// Runs a single turn of the agent using async channels.
///
/// Events are sent via a bounded `mpsc` channel for concurrent rendering
/// and thread persistence.
///
/// Returns the final assistant text and the updated message history.
pub async fn run_turn(
    messages: Vec<ChatMessage>,
    config: &Config,
    options: &AgentOptions,
    system_prompt: Option<&str>,
    thread_id: Option<&str>,
    tx: AgentEventTx,
) -> Result<(String, Vec<ChatMessage>)> {
    let sender = EventSender::new(tx);

    let selection = resolve_provider(&config.model);
    let provider = selection.kind;
    let max_tokens = config.effective_max_tokens_for(&config.model);
    let thinking_level = if crate::models::model_supports_reasoning(&config.model) {
        config.thinking_level
    } else {
        crate::config::ThinkingLevel::Off
    };

    // Get model output limit for budget calculation
    let model_output_limit = crate::models::ModelOption::find_by_id(&config.model)
        .map(|m| m.capabilities.output_limit)
        .filter(|&limit| limit > 0)
        .and_then(|limit| u32::try_from(limit).ok());

    let client = match provider {
        ProviderKind::Anthropic => {
            // Translate ThinkingLevel to raw API values
            let thinking_enabled = thinking_level.is_enabled();
            let thinking_budget_tokens = thinking_level
                .compute_reasoning_budget(max_tokens, model_output_limit)
                .unwrap_or(0);

            let anthropic_config = AnthropicConfig::from_env(
                selection.model.clone(),
                max_tokens,
                config.providers.anthropic.effective_base_url(),
                config.providers.anthropic.effective_api_key(),
                thinking_enabled,
                thinking_budget_tokens,
            )?;
            ProviderClient::Anthropic(AnthropicClient::new(anthropic_config))
        }
        ProviderKind::ClaudeCli => {
            let thinking_enabled = thinking_level.is_enabled();
            let thinking_budget_tokens = thinking_level
                .compute_reasoning_budget(max_tokens, model_output_limit)
                .unwrap_or(0);

            let claude_cli_config = ClaudeCliConfig::new(
                selection.model.clone(),
                max_tokens,
                config.providers.claude_cli.effective_base_url(),
                thinking_enabled,
                thinking_budget_tokens,
            );
            ProviderClient::ClaudeCli(ClaudeCliClient::new(claude_cli_config))
        }
        ProviderKind::OpenAICodex => {
            let reasoning_effort = map_thinking_to_reasoning(thinking_level);
            let cache_key = thread_id.map(|s| s.to_string());

            let openai_config = OpenAICodexConfig::new(
                selection.model.clone(),
                max_tokens,
                reasoning_effort,
                cache_key,
            );
            ProviderClient::OpenAICodex(OpenAICodexClient::new(openai_config))
        }
        ProviderKind::OpenAI => {
            let cache_key = thread_id.map(|s| s.to_string());

            let openai_config = OpenAIConfig::from_env(
                selection.model.clone(),
                max_tokens,
                config.providers.openai.effective_base_url(),
                config.providers.openai.effective_api_key(),
                cache_key,
            )?;
            ProviderClient::OpenAI(OpenAIClient::new(openai_config))
        }
        ProviderKind::OpenRouter => {
            let reasoning_effort = map_thinking_to_reasoning(thinking_level);
            let cache_key = thread_id.map(|s| s.to_string());
            let openrouter_config = OpenRouterConfig::from_env(
                selection.model.clone(),
                config.max_tokens,
                config.providers.openrouter.effective_base_url(),
                config.providers.openrouter.effective_api_key(),
                reasoning_effort,
                cache_key,
            )?;
            ProviderClient::OpenRouter(OpenRouterClient::new(openrouter_config))
        }
        ProviderKind::Mimo => {
            let thinking_enabled = thinking_level.is_enabled();
            let mimo_config = MimoConfig::from_env(
                selection.model.clone(),
                config.max_tokens,
                config.providers.mimo.effective_base_url(),
                config.providers.mimo.effective_api_key(),
                None,
                thinking_enabled,
            )?;
            ProviderClient::Mimo(MimoClient::new(mimo_config))
        }
        ProviderKind::Moonshot => {
            let cache_key = thread_id.map(|s| s.to_string());
            let thinking_enabled = thinking_level.is_enabled();
            let moonshot_config = MoonshotConfig::from_env(
                selection.model.clone(),
                config.max_tokens,
                config.providers.moonshot.effective_base_url(),
                config.providers.moonshot.effective_api_key(),
                cache_key,
                thinking_enabled,
            )?;
            ProviderClient::Moonshot(MoonshotClient::new(moonshot_config))
        }
        ProviderKind::Gemini => {
            // Map thinking level to Gemini-specific config (level for Gemini 3, budget for Gemini 2.5)
            let thinking_config = if thinking_level.is_enabled() {
                Some(GeminiThinkingConfig::from_thinking_level(
                    thinking_level,
                    &selection.model,
                ))
            } else {
                None
            };

            let gemini_config = GeminiConfig::from_env(
                selection.model.clone(),
                max_tokens,
                config.providers.gemini.effective_base_url(),
                config.providers.gemini.effective_api_key(),
                thinking_config,
            )?;
            ProviderClient::Gemini(GeminiClient::new(gemini_config))
        }
        ProviderKind::GeminiCli => {
            // Map thinking level to Gemini-specific config (level for Gemini 3, budget for Gemini 2.5)
            let thinking_config = if thinking_level.is_enabled() {
                Some(GeminiThinkingConfig::from_thinking_level(
                    thinking_level,
                    &selection.model,
                ))
            } else {
                None
            };

            let gemini_cli_config =
                GeminiCliConfig::new(selection.model.clone(), max_tokens, thinking_config);
            ProviderClient::GeminiCli(GeminiCliClient::new(gemini_cli_config))
        }
    };

    let tool_ctx = ToolContext::new(
        options.root.canonicalize().unwrap_or(options.root.clone()),
        config.tool_timeout(),
    )
    .with_config(config);

    let tool_registry = options.tool_config.registry.clone();

    // Cache tool definitions outside the loop (they're constant)
    let tools = match &options.tool_config.selection {
        ToolSelection::Auto { base, include } => {
            let provider_config = config.providers.get(provider);
            let base_tools = if provider_config.tools.is_some() {
                tool_registry.tools_for_provider(provider_config)
            } else {
                let tool_set =
                    if matches!(provider, ProviderKind::OpenAI | ProviderKind::OpenAICodex) {
                        ToolSet::OpenAICodex
                    } else {
                        *base
                    };
                tool_registry.tools_for_set(tool_set)
            };
            merge_tool_defs(base_tools, include, &tool_registry)
        }
        ToolSelection::ToolSet { base, include } => {
            let base_tools = tool_registry.tools_for_set(*base);
            merge_tool_defs(base_tools, include, &tool_registry)
        }
        ToolSelection::Explicit(names) => {
            tool_registry.tools_from_names(names.iter().map(String::as_str))
        }
        ToolSelection::All => tool_registry.definitions().to_vec(),
    };

    // Build the set of enabled tool names for validation in execute_tool
    // Keep canonical names (as defined) for proper display in error messages
    let enabled_tools: HashSet<String> = tools.iter().map(|t| t.name.clone()).collect();

    let mut messages = messages;

    // Tool loop - keep going until we get a final response
    loop {
        if interrupt::is_interrupted() {
            sender.send_important(AgentEvent::Interrupted).await;
            return Err(InterruptedError.into());
        }

        // Use select! to make the API call interruptible (important for slow responses
        // like Opus with extended thinking which can take 30+ seconds before first chunk)
        let stream_result = tokio::select! {
            biased;
            _ = interrupt::wait_for_interrupt() => {
                sender.send_important(AgentEvent::Interrupted).await;
                return Err(InterruptedError.into());
            }
            result = client.send_messages_stream(&messages, &tools, system_prompt) => result,
        };

        let mut stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                return Err(emit_error_async(e, &sender).await);
            }
        };

        // State for accumulating the current response
        let mut turn = AssistantTurnBuilder::new();
        let mut stop_reason: Option<String> = None;

        // Process stream events with periodic interrupt checking
        loop {
            if interrupt::is_interrupted() {
                sender.send_important(AgentEvent::Interrupted).await;
                return Err(InterruptedError.into());
            }

            // Use timeout to periodically check for interrupts even if stream stalls
            let next = timeout(STREAM_POLL_TIMEOUT, stream.next()).await;
            let event_result = match next {
                Ok(Some(result)) => result,
                Ok(None) => break,  // Stream ended
                Err(_) => continue, // Timeout, loop to re-check interrupt
            };

            let event = match event_result {
                Ok(e) => e,
                Err(e) => {
                    return Err(emit_error_async(anyhow::Error::new(e), &sender).await);
                }
            };

            match event {
                StreamEvent::TextDelta { text, .. } => {
                    if !text.is_empty() {
                        // Push to turn.text first, then move text into event (no clone)
                        turn.text.push_str(&text);
                        sender.send_delta(AgentEvent::AssistantDelta { text });
                    }
                }
                StreamEvent::ContentBlockStart {
                    index,
                    block_type,
                    id,
                    name,
                } => {
                    if block_type == ContentBlockType::ToolUse {
                        let tool_id = id.unwrap_or_default();
                        let tool_name = name.unwrap_or_default().to_ascii_lowercase();

                        // Emit ToolRequested immediately so UI shows the tool with a spinner
                        // while the JSON input is still streaming. This is especially important
                        // for tools like `write` where the content field can be very large.
                        sender
                            .send_important(AgentEvent::ToolRequested {
                                id: tool_id.clone(),
                                name: tool_name.clone(),
                                input: serde_json::json!({}),
                            })
                            .await;

                        turn.tool_uses.push(ToolUseBuilder {
                            index,
                            id: tool_id,
                            name: tool_name,
                            input_json: String::new(),
                            input_preview_len: 0,
                        });
                    } else if block_type == ContentBlockType::Reasoning {
                        turn.thinking_blocks.push(ThinkingBuilder {
                            index,
                            text: String::new(),
                            signature: String::new(),
                            replay: None,
                            had_delta: false,
                        });
                    }
                }
                StreamEvent::InputJsonDelta {
                    index,
                    partial_json,
                } => {
                    if let Some(tu) = turn.find_tool_use_mut(index) {
                        tu.input_json.push_str(&partial_json);
                        if let Some(delta) = extract_partial_tool_input(&tu.name, &tu.input_json)
                            && !delta.is_empty()
                            && delta.len() > tu.input_preview_len
                        {
                            tu.input_preview_len = delta.len();
                            sender
                                .send_important(AgentEvent::ToolInputDelta {
                                    id: tu.id.clone(),
                                    name: tu.name.clone(),
                                    delta,
                                })
                                .await;
                        }
                    }
                }
                StreamEvent::ReasoningDelta { index, reasoning } => {
                    if let Some(tb) = turn.find_thinking_mut(index) {
                        if !reasoning.is_empty() {
                            tb.had_delta = true;
                        }
                        tb.text.push_str(&reasoning);
                        sender.send_delta(AgentEvent::ReasoningDelta { text: reasoning });
                    }
                }
                StreamEvent::ReasoningSignatureDelta { index, signature } => {
                    if let Some(tb) = turn.find_thinking_mut(index) {
                        tb.signature.push_str(&signature);
                    }
                }
                StreamEvent::ContentBlockCompleted { index } => {
                    // Check if this is a thinking block finishing
                    if let Some(tb) = turn.find_thinking_mut(index) {
                        if tb.replay.is_none() && !tb.signature.is_empty() {
                            // Create provider-specific replay token
                            tb.replay = Some(match provider {
                                ProviderKind::Gemini | ProviderKind::GeminiCli => {
                                    ReplayToken::Gemini {
                                        signature: tb.signature.clone(),
                                    }
                                }
                                // Anthropic and Claude CLI use Anthropic format
                                _ => ReplayToken::Anthropic {
                                    signature: tb.signature.clone(),
                                },
                            });
                        }
                        let text = if tb.text.is_empty() {
                            None
                        } else {
                            Some(tb.text.clone())
                        };
                        let block = ReasoningBlock {
                            text,
                            replay: tb.replay.clone(),
                        };
                        sender
                            .send_important(AgentEvent::ReasoningCompleted { block })
                            .await;
                    }

                    // Check if this is a tool_use block finishing - emit ToolInputCompleted
                    // with the complete input for thread persistence.
                    if let Some(tu) = turn.tool_uses.iter().find(|t| t.index == index) {
                        // Try to parse the input JSON; if it fails, use empty object
                        // (the full error will be handled later when finalizing)
                        let input: Value = serde_json::from_str(&tu.input_json)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        sender
                            .send_important(AgentEvent::ToolInputCompleted {
                                id: tu.id.clone(),
                                name: tu.name.clone(),
                                input,
                            })
                            .await;
                    }
                }
                StreamEvent::MessageDelta {
                    stop_reason: reason,
                    usage,
                } => {
                    stop_reason = reason;
                    // Emit final output token count (message_delta has the total)
                    // Only emit output_tokens here to avoid double-counting with message_start
                    if let Some(u) = usage {
                        sender
                            .send_important(AgentEvent::UsageUpdate {
                                input_tokens: 0, // Already counted in message_start
                                output_tokens: u.output_tokens,
                                cache_read_input_tokens: 0, // Already counted in message_start
                                cache_creation_input_tokens: 0, // Already counted in message_start
                            })
                            .await;
                    }
                }
                StreamEvent::Error {
                    error_type,
                    message,
                } => {
                    let provider_err = ProviderError::api_error(&error_type, &message);
                    sender
                        .send_important(AgentEvent::Error {
                            kind: ErrorKind::ApiError,
                            message: provider_err.message.clone(),
                            details: provider_err.details.clone(),
                        })
                        .await;
                    return Err(anyhow::Error::new(provider_err));
                }
                StreamEvent::MessageStart { usage, .. } => {
                    // Emit initial usage: input tokens and cache info only
                    // Output tokens come from message_delta to avoid double-counting
                    sender
                        .send_important(AgentEvent::UsageUpdate {
                            input_tokens: usage.input_tokens,
                            output_tokens: 0, // Will be set by message_delta
                            cache_read_input_tokens: usage.cache_read_input_tokens,
                            cache_creation_input_tokens: usage.cache_creation_input_tokens,
                        })
                        .await;
                }
                StreamEvent::ReasoningCompleted {
                    index,
                    id,
                    encrypted_content,
                    summary,
                } => {
                    // Attach OpenAI replay data to the corresponding thinking builder
                    if let Some(tb) = turn.find_thinking_mut(index) {
                        // Use summary as fallback text if we didn't get any reasoning deltas
                        if !tb.had_delta
                            && tb.text.is_empty()
                            && let Some(s) = summary
                        {
                            tb.text = s;
                        }

                        tb.replay = Some(ReplayToken::OpenAI {
                            id: id.clone(),
                            encrypted_content: encrypted_content.clone(),
                        });
                    }
                }
                // Ignore other events (Ping, MessageCompleted)
                _ => {}
            }
        }

        // Check if we have tool use to process
        if stop_reason.as_deref() == Some("tool_use") && !turn.tool_uses.is_empty() {
            // Finalize all tool uses (parse JSON once)
            let mut finalized = Vec::with_capacity(turn.tool_uses.len());
            for tu in turn.tool_uses.drain(..) {
                match tu.clone().finalize() {
                    Ok(tool_use) => finalized.push(tool_use),
                    Err(e) => {
                        // Emit structured error for invalid JSON
                        sender
                            .send_important(AgentEvent::Error {
                                kind: ErrorKind::Parse,
                                message: format!("Invalid tool input JSON for {}: {}", tu.name, e),
                                details: Some(tu.input_json),
                            })
                            .await;
                        bail!("Invalid tool input JSON for {}: {}", tu.name, e);
                    }
                }
            }

            // Emit AssistantCompleted to signal this message is complete
            // This allows the TUI to finalize the current streaming cell before tools
            if !turn.text.is_empty() {
                sender
                    .send_important(AgentEvent::AssistantCompleted {
                        text: turn.text.clone(),
                    })
                    .await;
            }

            // Note: ToolRequested events are already emitted during streaming
            // (at ContentBlockStart for each tool_use block) for immediate UI feedback.

            // Build the assistant response with thinking + reasoning + tool_use blocks
            let turn_text = turn.text.clone();
            let assistant_blocks = turn.into_blocks(finalized.clone());
            messages.push(ChatMessage::assistant_blocks(assistant_blocks));

            // Execute tools and get results (may be partial on interrupt)
            let tool_results = execute_tools_async(
                &finalized,
                &tool_ctx,
                &enabled_tools,
                &sender,
                &tool_registry,
            )
            .await;
            messages.push(ChatMessage::tool_results(tool_results));

            // Check if interrupted during tool execution
            if interrupt::is_interrupted() {
                // Emit TurnCompleted with partial messages before Interrupted
                // This ensures the TUI has the complete thread state
                sender
                    .send_important(AgentEvent::TurnCompleted {
                        final_text: turn_text.clone(),
                        messages: messages.clone(),
                    })
                    .await;
                sender.send_important(AgentEvent::Interrupted).await;
                return Err(InterruptedError.into());
            }

            // Continue the loop for the next response
            continue;
        }

        // Emit final assistant text
        if !turn.text.is_empty() {
            sender
                .send_important(AgentEvent::AssistantCompleted {
                    text: turn.text.clone(),
                })
                .await;
        }

        // Build final assistant message with thinking + reasoning + text blocks
        let final_text = turn.text.clone();
        let assistant_blocks = turn.into_blocks(vec![]);
        if !assistant_blocks.is_empty() {
            messages.push(ChatMessage::assistant_blocks(assistant_blocks));
        }

        // Emit turn complete with final result
        sender
            .send_important(AgentEvent::TurnCompleted {
                final_text: final_text.clone(),
                messages: messages.clone(),
            })
            .await;

        return Ok((final_text, messages));
    }
}

fn map_thinking_to_reasoning(level: crate::config::ThinkingLevel) -> Option<String> {
    level.effort_label().map(|s| s.to_string())
}

/// Executes all tool uses in parallel and emits events via async channel.
///
/// Tools are spawned concurrently using `tokio::JoinSet`. ToolStarted events
/// are emitted sequentially before spawning to preserve CLI output order.
/// ToolCompleted events are emitted as each task completes.
///
/// On interrupt, aborts all remaining tasks and emits abort results for
/// incomplete tools. The caller should check `is_interrupted()` after this
/// function returns to determine if an interrupt occurred.
async fn execute_tools_async(
    tool_uses: &[ToolUse],
    ctx: &ToolContext,
    enabled_tools: &HashSet<String>,
    sender: &EventSender,
    tool_registry: &ToolRegistry,
) -> Vec<ToolResult> {
    let mut join_set: JoinSet<(usize, String, ToolOutput, ToolResult)> = JoinSet::new();
    let mut results: Vec<Option<(ToolOutput, ToolResult)>> = vec![None; tool_uses.len()];
    let mut completed: HashSet<usize> = HashSet::new();

    // Emit ToolStarted sequentially, then spawn tasks
    for (i, tu) in tool_uses.iter().enumerate() {
        sender
            .send_important(AgentEvent::ToolStarted {
                id: tu.id.clone(),
                name: tu.name.clone(),
            })
            .await;

        // Clone for 'static requirement
        let tu = tu.clone();
        let ctx = ctx.clone();
        let enabled_tools = enabled_tools.clone();
        let tool_registry = tool_registry.clone();

        join_set.spawn(async move {
            let (output, result) = tool_registry
                .execute_tool(&tu.name, &tu.id, &tu.input, &ctx, &enabled_tools)
                .await;
            (i, tu.id.clone(), output, result)
        });
    }

    // Collect results with interrupt handling
    loop {
        tokio::select! {
            biased;
            _ = interrupt::wait_for_interrupt() => {
                // Abort all remaining tasks
                join_set.abort_all();

                // Drain any already-completed tasks to avoid missing results
                while let Some(task_result) = join_set.try_join_next() {
                    if let Ok((idx, id, output, result)) = task_result
                        && !completed.contains(&idx)
                    {
                        completed.insert(idx);
                        sender.send_important(AgentEvent::ToolCompleted {
                            id,
                            result: output.clone(),
                        })
                        .await;
                        results[idx] = Some((output, result));
                    }
                }

                // Emit abort for incomplete tools
                for (i, tu) in tool_uses.iter().enumerate() {
                    if !completed.contains(&i) {
                        let abort_output = ToolOutput::canceled("Interrupted by user");
                        sender.send_important(AgentEvent::ToolCompleted {
                            id: tu.id.clone(),
                            result: abort_output.clone(),
                        })
                        .await;
                        results[i] = Some((
                            abort_output.clone(),
                            ToolResult::from_output(tu.id.clone(), &abort_output),
                        ));
                    }
                }
                break;
            }
            task_result = join_set.join_next() => {
                match task_result {
                    Some(Ok((idx, id, output, result))) => {
                        completed.insert(idx);
                        sender.send_important(AgentEvent::ToolCompleted {
                            id,
                            result: output.clone(),
                        })
                        .await;
                        results[idx] = Some((output, result));
                    }
                    Some(Err(e)) => {
                        // JoinError: panic or cancellation
                        // This is rare and typically only happens if a task panics.
                        // Log it but continue - the slot will remain None and be
                        // caught by the expect below (which is a bug if it happens).
                        eprintln!("Task join error: {:?}", e);
                    }
                    None => break, // All tasks completed
                }
            }
        }
    }

    // Convert to Vec<ToolResult>, unwrapping Options
    results
        .into_iter()
        .map(|opt| opt.expect("all slots should be filled").1)
        .collect()
}

/// Builds assistant content blocks from accumulated thinking, reasoning, text, and tool uses.
#[cfg(test)]
mod tests {
    use tokio::time::{Duration, timeout};

    use super::*;

    /// Verifies agent emits ToolStarted and ToolCompleted events (SPEC §7).
    #[tokio::test]
    async fn test_execute_tools_emits_events() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("test.txt"), "hello").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let enabled_tools: HashSet<String> = vec!["Read".to_string()].into_iter().collect();

        // Use ToolUse (finalized) instead of ToolUseBuilder
        let tool_uses = vec![ToolUse {
            id: "tool1".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"path": "test.txt"}),
        }];

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        // Run in a task so we can collect events
        let tool_registry = ToolRegistry::builtins();
        let handle = tokio::spawn(async move {
            execute_tools_async(&tool_uses, &ctx, &enabled_tools, &sender, &tool_registry).await
        });

        // Collect events with timeout to avoid hangs
        let mut received = Vec::new();
        for _ in 0..2 {
            let ev = timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timeout waiting for event")
                .expect("channel closed unexpectedly");
            received.push((*ev).clone());
        }

        let results = handle.await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(received.len(), 2); // ToolStarted, ToolCompleted

        assert!(matches!(&received[0], AgentEvent::ToolStarted { id, name }
            if id == "tool1" && name == "read"));
        assert!(
            matches!(&received[1], AgentEvent::ToolCompleted { id, result }
            if id == "tool1" && result.is_ok())
        );
    }

    /// Verifies channel is properly closed when sender is dropped.
    #[tokio::test]
    async fn test_event_channel_closes_on_sender_drop() {
        let (tx, mut rx) = create_event_channel();

        // Send one event then drop sender
        tx.send(Arc::new(AgentEvent::AssistantDelta {
            text: "hello".to_string(),
        }))
        .await
        .unwrap();
        drop(tx);

        // Should receive the event
        let ev = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timeout")
            .unwrap();
        assert!(matches!(&*ev, AgentEvent::AssistantDelta { text } if text == "hello"));

        // Should get None when channel is closed
        assert!(rx.recv().await.is_none());
    }

    /// Verifies EventSender send_delta() is best-effort (doesn't block on full channel).
    #[tokio::test]
    async fn test_event_sender_send_delta_is_best_effort() {
        // Create a tiny channel that will fill up quickly
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let sender = EventSender::new(tx);

        // This should not block even though channel is tiny
        for i in 0..100 {
            sender.send_delta(AgentEvent::AssistantDelta {
                text: format!("chunk {}", i),
            });
        }
        // If we got here without blocking, the test passes
    }

    /// Verifies ToolUseBuilder finalization fails on invalid JSON.
    #[tokio::test]
    async fn test_tool_use_builder_finalize_fails_on_invalid_json() {
        let builder = ToolUseBuilder {
            index: 0,
            id: "tool1".to_string(),
            name: "test".to_string(),
            input_json: "{invalid json}".to_string(),
            input_preview_len: 0,
        };

        let result = builder.finalize();
        assert!(result.is_err());
    }

    /// Verifies broadcaster removes closed channels.
    #[tokio::test]
    async fn test_broadcaster_removes_closed_channels() {
        let (source_tx, source_rx) = create_event_channel();
        let (out1_tx, mut out1_rx) = create_event_channel();
        let (out2_tx, out2_rx) = create_event_channel();

        // Drop out2's receiver immediately
        drop(out2_rx);

        let _broadcaster = spawn_broadcaster(source_rx, vec![out1_tx, out2_tx]);

        // Send an event
        source_tx
            .send(Arc::new(AgentEvent::AssistantDelta {
                text: "test".to_string(),
            }))
            .await
            .unwrap();

        // out1 should receive it
        let ev = timeout(Duration::from_secs(1), out1_rx.recv())
            .await
            .expect("timeout")
            .expect("should receive event");
        assert!(matches!(&*ev, AgentEvent::AssistantDelta { text } if text == "test"));
    }
}
