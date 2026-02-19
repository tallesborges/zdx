//! Agent module for UI-agnostic execution.
//!
//! The agent drives the provider + tool loop and emits `AgentEvent`s
//! via async channels. No direct stdout/stderr writes occur in this module.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, timeout};

use crate::config::{Config, ThinkingLevel};
use crate::core::events::{AgentEvent, ErrorKind, ToolOutput};
use crate::core::interrupt::{self, InterruptedError};
use crate::providers::anthropic::{
    AnthropicClient, AnthropicConfig, ClaudeCliClient, ClaudeCliConfig,
    EffortLevel as AnthropicEffortLevel,
};
use crate::providers::apiyi::{ApiyiClient, ApiyiConfig};
use crate::providers::gemini::{
    GeminiCliClient, GeminiCliConfig, GeminiClient, GeminiConfig, GeminiThinkingConfig,
};
use crate::providers::mimo::{MimoClient, MimoConfig};
use crate::providers::mistral::{MistralClient, MistralConfig};
use crate::providers::moonshot::{MoonshotClient, MoonshotConfig};
use crate::providers::openai::{OpenAIClient, OpenAICodexClient, OpenAICodexConfig, OpenAIConfig};
use crate::providers::openrouter::{OpenRouterClient, OpenRouterConfig};
use crate::providers::stepfun::{StepfunClient, StepfunConfig};
use crate::providers::zen::{ZenClient, ZenConfig};
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

    #[must_use]
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
/// Use `send_delta()` for high-volume events (`TextDelta`) that can be dropped
/// if the consumer is slow. Use `send_important()` for events that must be
/// delivered (`ToolStarted`, `ToolCompleted`, Completed, Error, Interrupted).
#[derive(Clone)]
pub struct EventSender {
    tx: AgentEventTx,
}

impl EventSender {
    /// Creates a new `EventSender` wrapping the given channel sender.
    pub fn new(tx: AgentEventTx) -> Self {
        Self { tx }
    }

    /// Best-effort send: never awaits, drops if channel is full.
    /// Use for high-volume events like `TextDelta` that can afford loss.
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
    Mistral(MistralClient),
    Moonshot(MoonshotClient),
    Stepfun(StepfunClient),
    Gemini(GeminiClient),
    GeminiCli(GeminiCliClient),
    Zen(ZenClient),
    Apiyi(ApiyiClient),
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
            ProviderClient::Mistral(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::Moonshot(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::Stepfun(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::Gemini(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::GeminiCli(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::Zen(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::Apiyi(client) => {
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
                match tx.try_send(Arc::clone(&event)) {
                    Ok(()) | Err(TrySendError::Full(_)) => true, // drop this event, keep channel
                    Err(TrySendError::Closed(_)) => false,       // remove closed channel
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

    /// Converts accumulated content into `ChatContentBlocks` for API messages.
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
    ///
    /// # Errors
    /// Returns an error if the operation fails.
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
    let key = format!("\"{field}\"");
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
/// This preserves the full error chain (including `ProviderError` details) for callers.
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

/// Truncates a string for error reporting to avoid bloating logs and model context.
fn truncate_for_error(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}... (truncated, {} total bytes)", &s[..max_len], s.len())
    }
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
/// Abort threshold for repeated malformed-only tool turns.
const MAX_CONSECUTIVE_MALFORMED_TOOL_TURNS: usize = 3;
const MALFORMED_TOOL_LOOP_ABORT_MESSAGE: &str =
    "Aborting after repeated malformed tool calls with invalid JSON input";

/// Runs a single turn of the agent using async channels.
///
/// Events are sent via a bounded `mpsc` channel for concurrent rendering
/// and thread persistence.
///
/// Returns the final assistant text and the updated message history.
///
/// # Errors
/// Returns an error if the operation fails.
pub async fn run_turn(
    messages: Vec<ChatMessage>,
    config: &Config,
    options: &AgentOptions,
    system_prompt: Option<&str>,
    thread_id: Option<&str>,
    tx: AgentEventTx,
) -> Result<(String, Vec<ChatMessage>)> {
    let sender = EventSender::new(tx);
    let setup = build_run_turn_setup(config, options, thread_id)?;
    let mut messages = messages;
    let mut consecutive_malformed_tool_turns = 0usize;

    loop {
        ensure_not_interrupted(&sender, None).await?;
        let stream = request_stream(
            &setup.client,
            &messages,
            &setup.tools,
            system_prompt,
            &sender,
        )
        .await?;
        let mut stream_state = consume_stream(stream, setup.provider, &sender).await?;

        if stream_state.needs_tool_execution() {
            let stats =
                process_tool_turn(&mut messages, &mut stream_state.turn, &setup, &sender).await?;
            if stats.executable == 0 && stats.malformed > 0 {
                consecutive_malformed_tool_turns += 1;
                if consecutive_malformed_tool_turns >= MAX_CONSECUTIVE_MALFORMED_TOOL_TURNS {
                    sender
                        .send_important(AgentEvent::Error {
                            kind: ErrorKind::Parse,
                            message: MALFORMED_TOOL_LOOP_ABORT_MESSAGE.to_string(),
                            details: Some(
                                "Provider repeatedly requested tool calls without valid arguments"
                                    .to_string(),
                            ),
                        })
                        .await;
                    return Err(anyhow!(MALFORMED_TOOL_LOOP_ABORT_MESSAGE));
                }
            } else {
                consecutive_malformed_tool_turns = 0;
            }
            continue;
        }

        return finalize_non_tool_turn(&mut messages, stream_state.turn, &sender).await;
    }
}

struct RunTurnSetup {
    provider: ProviderKind,
    client: ProviderClient,
    tools: Vec<ToolDefinition>,
    enabled_tools: HashSet<String>,
    tool_ctx: ToolContext,
    tool_registry: ToolRegistry,
}

fn build_run_turn_setup(
    config: &Config,
    options: &AgentOptions,
    thread_id: Option<&str>,
) -> Result<RunTurnSetup> {
    let selection = resolve_provider(&config.model);
    let provider = selection.kind;
    let max_tokens = config.effective_max_tokens_for(&config.model);
    let thinking_level = if crate::models::model_supports_reasoning(&config.model) {
        config.thinking_level
    } else {
        ThinkingLevel::Off
    };
    let model_output_limit = crate::models::ModelOption::find_by_id(&config.model)
        .map(|m| m.capabilities.output_limit)
        .filter(|&limit| limit > 0)
        .and_then(|limit| u32::try_from(limit).ok());

    let client = build_provider_client(
        config,
        thread_id,
        &selection.model,
        provider,
        max_tokens,
        thinking_level,
        model_output_limit,
    )?;
    let tool_ctx = ToolContext::new(
        options.root.canonicalize().unwrap_or(options.root.clone()),
        config.tool_timeout(),
    )
    .with_config(config);
    let tool_registry = options.tool_config.registry.clone();
    let tools = resolve_tools(config, options, provider, &tool_registry);
    let enabled_tools = tools.iter().map(|t| t.name.clone()).collect();

    Ok(RunTurnSetup {
        provider,
        client,
        tools,
        enabled_tools,
        tool_ctx,
        tool_registry,
    })
}

fn build_provider_client(
    config: &Config,
    thread_id: Option<&str>,
    model: &str,
    provider: ProviderKind,
    max_tokens: u32,
    thinking_level: ThinkingLevel,
    model_output_limit: Option<u32>,
) -> Result<ProviderClient> {
    let cache_key = thread_id.map(std::string::ToString::to_string);
    let thinking_enabled = thinking_level.is_enabled();
    let reasoning_effort = map_thinking_to_reasoning(thinking_level);
    let thinking_budget_tokens = thinking_level
        .compute_reasoning_budget(max_tokens, model_output_limit)
        .unwrap_or(0);
    let thinking_effort = map_thinking_to_anthropic_effort(thinking_level);
    let gemini_thinking =
        thinking_enabled.then(|| GeminiThinkingConfig::from_thinking_level(thinking_level, model));

    match provider {
        ProviderKind::Anthropic => build_anthropic_client(
            config,
            model,
            max_tokens,
            thinking_enabled,
            thinking_budget_tokens,
            thinking_effort,
        ),
        ProviderKind::ClaudeCli => Ok(ProviderClient::ClaudeCli(ClaudeCliClient::new(
            ClaudeCliConfig::new(
                model.to_string(),
                max_tokens,
                config.providers.claude_cli.effective_base_url(),
                thinking_enabled,
                thinking_budget_tokens,
                thinking_effort,
            ),
        ))),
        ProviderKind::OpenAICodex => Ok(ProviderClient::OpenAICodex(OpenAICodexClient::new(
            OpenAICodexConfig::new(model.to_string(), max_tokens, reasoning_effort, cache_key),
        ))),
        ProviderKind::OpenAI => build_openai_client(config, model, max_tokens, cache_key),
        ProviderKind::OpenRouter => {
            build_openrouter_client(config, model, reasoning_effort, cache_key)
        }
        ProviderKind::Mimo => build_mimo_client(config, model, thinking_enabled),
        ProviderKind::Mistral => build_mistral_client(config, model, cache_key, thinking_enabled),
        ProviderKind::Moonshot => build_moonshot_client(config, model, cache_key, thinking_enabled),
        ProviderKind::Stepfun => build_stepfun_client(config, model, cache_key, thinking_enabled),
        ProviderKind::Gemini => build_gemini_client(config, model, max_tokens, gemini_thinking),
        ProviderKind::GeminiCli => Ok(ProviderClient::GeminiCli(GeminiCliClient::new(
            GeminiCliConfig::new(model.to_string(), max_tokens, gemini_thinking),
        ))),
        ProviderKind::Zen => build_zen_client(
            config,
            model,
            max_tokens,
            thinking_enabled,
            thinking_budget_tokens,
            thinking_effort,
            gemini_thinking.clone(),
            reasoning_effort,
            cache_key,
        ),
        ProviderKind::Apiyi => build_apiyi_client(
            config,
            model,
            max_tokens,
            thinking_enabled,
            thinking_budget_tokens,
            thinking_effort,
            gemini_thinking.clone(),
            reasoning_effort,
            cache_key,
        ),
    }
}

fn build_anthropic_client(
    config: &Config,
    model: &str,
    max_tokens: u32,
    thinking_enabled: bool,
    thinking_budget_tokens: u32,
    thinking_effort: Option<AnthropicEffortLevel>,
) -> Result<ProviderClient> {
    Ok(ProviderClient::Anthropic(AnthropicClient::new(
        AnthropicConfig::from_env(
            model.to_string(),
            max_tokens,
            config.providers.anthropic.effective_base_url(),
            config.providers.anthropic.effective_api_key(),
            thinking_enabled,
            thinking_budget_tokens,
            thinking_effort,
        )?,
    )))
}

fn build_openai_client(
    config: &Config,
    model: &str,
    max_tokens: u32,
    cache_key: Option<String>,
) -> Result<ProviderClient> {
    Ok(ProviderClient::OpenAI(OpenAIClient::new(
        OpenAIConfig::from_env(
            model.to_string(),
            max_tokens,
            config.providers.openai.effective_base_url(),
            config.providers.openai.effective_api_key(),
            cache_key,
        )?,
    )))
}

fn build_openrouter_client(
    config: &Config,
    model: &str,
    reasoning_effort: Option<String>,
    cache_key: Option<String>,
) -> Result<ProviderClient> {
    Ok(ProviderClient::OpenRouter(OpenRouterClient::new(
        OpenRouterConfig::from_env(
            model.to_string(),
            config.max_tokens,
            config.providers.openrouter.effective_base_url(),
            config.providers.openrouter.effective_api_key(),
            reasoning_effort,
            cache_key,
        )?,
    )))
}

fn build_mimo_client(
    config: &Config,
    model: &str,
    thinking_enabled: bool,
) -> Result<ProviderClient> {
    Ok(ProviderClient::Mimo(MimoClient::new(MimoConfig::from_env(
        model.to_string(),
        config.max_tokens,
        config.providers.mimo.effective_base_url(),
        config.providers.mimo.effective_api_key(),
        None,
        thinking_enabled,
    )?)))
}

fn build_mistral_client(
    config: &Config,
    model: &str,
    cache_key: Option<String>,
    thinking_enabled: bool,
) -> Result<ProviderClient> {
    Ok(ProviderClient::Mistral(MistralClient::new(
        MistralConfig::from_env(
            model.to_string(),
            config.max_tokens,
            config.providers.mistral.effective_base_url(),
            config.providers.mistral.effective_api_key(),
            cache_key,
            thinking_enabled,
        )?,
    )))
}

fn build_moonshot_client(
    config: &Config,
    model: &str,
    cache_key: Option<String>,
    thinking_enabled: bool,
) -> Result<ProviderClient> {
    Ok(ProviderClient::Moonshot(MoonshotClient::new(
        MoonshotConfig::from_env(
            model.to_string(),
            config.max_tokens,
            config.providers.moonshot.effective_base_url(),
            config.providers.moonshot.effective_api_key(),
            cache_key,
            thinking_enabled,
        )?,
    )))
}

fn build_stepfun_client(
    config: &Config,
    model: &str,
    cache_key: Option<String>,
    thinking_enabled: bool,
) -> Result<ProviderClient> {
    Ok(ProviderClient::Stepfun(StepfunClient::new(
        StepfunConfig::from_env(
            model.to_string(),
            config.max_tokens,
            config.providers.stepfun.effective_base_url(),
            config.providers.stepfun.effective_api_key(),
            cache_key,
            thinking_enabled,
        )?,
    )))
}

fn build_gemini_client(
    config: &Config,
    model: &str,
    max_tokens: u32,
    gemini_thinking: Option<GeminiThinkingConfig>,
) -> Result<ProviderClient> {
    Ok(ProviderClient::Gemini(GeminiClient::new(
        GeminiConfig::from_env(
            model.to_string(),
            max_tokens,
            config.providers.gemini.effective_base_url(),
            config.providers.gemini.effective_api_key(),
            gemini_thinking,
        )?,
    )))
}

#[allow(clippy::too_many_arguments)]
fn build_zen_client(
    config: &Config,
    model: &str,
    max_tokens: u32,
    thinking_enabled: bool,
    thinking_budget_tokens: u32,
    thinking_effort: Option<AnthropicEffortLevel>,
    gemini_thinking: Option<GeminiThinkingConfig>,
    reasoning_effort: Option<String>,
    cache_key: Option<String>,
) -> Result<ProviderClient> {
    Ok(ProviderClient::Zen(ZenClient::new(ZenConfig::from_env(
        model.to_string(),
        max_tokens,
        config.providers.zen.effective_base_url(),
        config.providers.zen.effective_api_key(),
        thinking_enabled,
        thinking_budget_tokens,
        thinking_effort,
        gemini_thinking,
        reasoning_effort,
        cache_key,
    )?)))
}

#[allow(clippy::too_many_arguments)]
fn build_apiyi_client(
    config: &Config,
    model: &str,
    max_tokens: u32,
    thinking_enabled: bool,
    thinking_budget_tokens: u32,
    thinking_effort: Option<AnthropicEffortLevel>,
    gemini_thinking: Option<GeminiThinkingConfig>,
    reasoning_effort: Option<String>,
    cache_key: Option<String>,
) -> Result<ProviderClient> {
    Ok(ProviderClient::Apiyi(ApiyiClient::new(
        ApiyiConfig::from_env(
            model.to_string(),
            max_tokens,
            config.providers.apiyi.effective_base_url(),
            config.providers.apiyi.effective_api_key(),
            thinking_enabled,
            thinking_budget_tokens,
            thinking_effort,
            gemini_thinking,
            reasoning_effort,
            cache_key,
        )?,
    )))
}

fn resolve_tools(
    config: &Config,
    options: &AgentOptions,
    provider: ProviderKind,
    tool_registry: &ToolRegistry,
) -> Vec<ToolDefinition> {
    let mut tools = match &options.tool_config.selection {
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
            merge_tool_defs(base_tools, include, tool_registry)
        }
        ToolSelection::ToolSet { base, include } => {
            merge_tool_defs(tool_registry.tools_for_set(*base), include, tool_registry)
        }
        ToolSelection::Explicit(names) => {
            tool_registry.tools_from_names(names.iter().map(String::as_str))
        }
        ToolSelection::All => tool_registry.definitions().to_vec(),
    };

    if !config.subagents.enabled {
        tools.retain(|tool| !tool.name.eq_ignore_ascii_case("Invoke_Subagent"));
    }

    tools
}

struct StreamState {
    turn: AssistantTurnBuilder,
    stop_reason: Option<String>,
}

impl StreamState {
    fn new() -> Self {
        Self {
            turn: AssistantTurnBuilder::new(),
            stop_reason: None,
        }
    }

    fn needs_tool_execution(&self) -> bool {
        self.stop_reason.as_deref() == Some("tool_use") && !self.turn.tool_uses.is_empty()
    }
}

async fn ensure_not_interrupted(
    sender: &EventSender,
    partial_content: Option<String>,
) -> Result<()> {
    if interrupt::is_interrupted() {
        sender
            .send_important(AgentEvent::Interrupted { partial_content })
            .await;
        return Err(InterruptedError.into());
    }
    Ok(())
}

async fn request_stream(
    client: &ProviderClient,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    system_prompt: Option<&str>,
    sender: &EventSender,
) -> Result<ProviderStream> {
    let stream_result = tokio::select! {
        biased;
        () = interrupt::wait_for_interrupt() => {
            sender.send_important(AgentEvent::Interrupted { partial_content: None }).await;
            return Err(InterruptedError.into());
        }
        result = client.send_messages_stream(messages, tools, system_prompt) => result,
    };
    match stream_result {
        Ok(stream) => Ok(stream),
        Err(err) => Err(emit_error_async(err, sender).await),
    }
}

async fn consume_stream(
    mut stream: ProviderStream,
    provider: ProviderKind,
    sender: &EventSender,
) -> Result<StreamState> {
    let mut state = StreamState::new();

    loop {
        let partial = (!state.turn.text.is_empty()).then(|| state.turn.text.clone());
        ensure_not_interrupted(sender, partial).await?;
        let event = match timeout(STREAM_POLL_TIMEOUT, stream.next()).await {
            Ok(Some(result)) => result.map_err(anyhow::Error::new)?,
            Ok(None) => return Ok(state),
            Err(_) => continue,
        };
        handle_stream_event(event, provider, sender, &mut state).await?;
    }
}

async fn handle_stream_event(
    event: StreamEvent,
    provider: ProviderKind,
    sender: &EventSender,
    state: &mut StreamState,
) -> Result<()> {
    match event {
        StreamEvent::TextDelta { text, .. } if !text.is_empty() => {
            state.turn.text.push_str(&text);
            sender.send_delta(AgentEvent::AssistantDelta { text });
        }
        StreamEvent::ContentBlockStart {
            index,
            block_type: ContentBlockType::ToolUse,
            id,
            name,
        } => {
            handle_tool_content_start(index, id, name, sender, &mut state.turn).await;
        }
        StreamEvent::ContentBlockStart {
            index,
            block_type: ContentBlockType::Reasoning,
            ..
        } => {
            state.turn.thinking_blocks.push(ThinkingBuilder {
                index,
                text: String::new(),
                signature: String::new(),
                replay: None,
                had_delta: false,
            });
        }
        StreamEvent::InputJsonDelta {
            index,
            partial_json,
        } => handle_input_json_delta(index, partial_json, sender, &mut state.turn).await,
        StreamEvent::ReasoningDelta { index, reasoning } => {
            if let Some(tb) = state.turn.find_thinking_mut(index) {
                if !reasoning.is_empty() {
                    tb.had_delta = true;
                }
                tb.text.push_str(&reasoning);
                sender.send_delta(AgentEvent::ReasoningDelta { text: reasoning });
            }
        }
        StreamEvent::ReasoningSignatureDelta { index, signature } => {
            if let Some(tb) = state.turn.find_thinking_mut(index) {
                tb.signature.push_str(&signature);
            }
        }
        StreamEvent::ContentBlockCompleted { index } => {
            emit_reasoning_completion(provider, sender, &mut state.turn, index).await;
            emit_tool_input_completion(sender, &state.turn, index).await;
        }
        StreamEvent::MessageDelta { stop_reason, usage } => {
            state.stop_reason = stop_reason;
            emit_message_delta_usage(sender, usage).await;
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
        StreamEvent::MessageStart { usage, .. } => emit_message_start_usage(sender, usage).await,
        StreamEvent::ReasoningCompleted {
            index,
            id,
            encrypted_content,
            summary,
        } => apply_openai_reasoning_completion(
            &mut state.turn,
            index,
            id,
            encrypted_content,
            summary,
        ),
        _ => {}
    }
    Ok(())
}

async fn handle_tool_content_start(
    index: usize,
    id: Option<String>,
    name: Option<String>,
    sender: &EventSender,
    turn: &mut AssistantTurnBuilder,
) {
    let tool_id = id.unwrap_or_default();
    let tool_name = name.unwrap_or_default().to_ascii_lowercase();
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
}

async fn handle_input_json_delta(
    index: usize,
    partial_json: String,
    sender: &EventSender,
    turn: &mut AssistantTurnBuilder,
) {
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

async fn emit_message_delta_usage(sender: &EventSender, usage: Option<crate::providers::Usage>) {
    if let Some(u) = usage {
        sender
            .send_important(AgentEvent::UsageUpdate {
                input_tokens: 0,
                output_tokens: u.output_tokens,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })
            .await;
    }
}

async fn emit_message_start_usage(sender: &EventSender, usage: crate::providers::Usage) {
    sender
        .send_important(AgentEvent::UsageUpdate {
            input_tokens: usage.input_tokens,
            output_tokens: 0,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
        })
        .await;
}

fn apply_openai_reasoning_completion(
    turn: &mut AssistantTurnBuilder,
    index: usize,
    id: String,
    encrypted_content: String,
    summary: Option<String>,
) {
    if let Some(tb) = turn.find_thinking_mut(index) {
        if !tb.had_delta
            && tb.text.is_empty()
            && let Some(s) = summary
        {
            tb.text = s;
        }
        tb.replay = Some(ReplayToken::OpenAI {
            id,
            encrypted_content,
        });
    }
}

async fn emit_reasoning_completion(
    provider: ProviderKind,
    sender: &EventSender,
    turn: &mut AssistantTurnBuilder,
    index: usize,
) {
    if let Some(tb) = turn.find_thinking_mut(index) {
        if tb.replay.is_none() && !tb.signature.is_empty() {
            tb.replay = Some(match provider {
                ProviderKind::Gemini | ProviderKind::GeminiCli => ReplayToken::Gemini {
                    signature: tb.signature.clone(),
                },
                _ => ReplayToken::Anthropic {
                    signature: tb.signature.clone(),
                },
            });
        }
        let block = ReasoningBlock {
            text: (!tb.text.is_empty()).then(|| tb.text.clone()),
            replay: tb.replay.clone(),
        };
        sender
            .send_important(AgentEvent::ReasoningCompleted { block })
            .await;
    }
}

async fn emit_tool_input_completion(
    sender: &EventSender,
    turn: &AssistantTurnBuilder,
    index: usize,
) {
    if let Some(tu) = turn.tool_uses.iter().find(|t| t.index == index) {
        let input: Value =
            serde_json::from_str(&tu.input_json).unwrap_or_else(|_| serde_json::json!({}));
        sender
            .send_important(AgentEvent::ToolInputCompleted {
                id: tu.id.clone(),
                name: tu.name.clone(),
                input,
            })
            .await;
    }
}

struct ToolTurnOutcome {
    executable: Vec<ToolUse>,
    assistant_tools: Vec<ToolUse>,
    malformed_results: Vec<ToolResult>,
    malformed_tools: Vec<(String, String, ToolOutput)>,
}

struct ToolTurnStats {
    executable: usize,
    malformed: usize,
}

async fn process_tool_turn(
    messages: &mut Vec<ChatMessage>,
    turn: &mut AssistantTurnBuilder,
    setup: &RunTurnSetup,
    sender: &EventSender,
) -> Result<ToolTurnStats> {
    let outcome = finalize_tool_calls(turn, sender).await;
    let executable_count = outcome.executable.len();
    let malformed_count = outcome.malformed_results.len();
    emit_assistant_completed_if_present(sender, &turn.text).await;
    emit_malformed_tool_events(sender, outcome.malformed_tools).await;

    let turn_text = turn.text.clone();
    messages.push(ChatMessage::assistant_blocks(
        std::mem::take(turn).into_blocks(outcome.assistant_tools),
    ));

    let mut tool_results = execute_tools_async(
        &outcome.executable,
        &setup.tool_ctx,
        &setup.enabled_tools,
        sender,
        &setup.tool_registry,
    )
    .await;
    tool_results.extend(outcome.malformed_results);
    messages.push(ChatMessage::tool_results(tool_results));

    if interrupt::is_interrupted() {
        sender
            .send_important(AgentEvent::TurnCompleted {
                final_text: turn_text.clone(),
                messages: messages.clone(),
            })
            .await;
        sender
            .send_important(AgentEvent::Interrupted {
                partial_content: (!turn_text.is_empty()).then_some(turn_text),
            })
            .await;
        return Err(InterruptedError.into());
    }

    Ok(ToolTurnStats {
        executable: executable_count,
        malformed: malformed_count,
    })
}

async fn finalize_non_tool_turn(
    messages: &mut Vec<ChatMessage>,
    turn: AssistantTurnBuilder,
    sender: &EventSender,
) -> Result<(String, Vec<ChatMessage>)> {
    let final_text = turn.text.clone();
    emit_assistant_completed_if_present(sender, &final_text).await;
    let assistant_blocks = turn.into_blocks(Vec::new());
    if !assistant_blocks.is_empty() {
        messages.push(ChatMessage::assistant_blocks(assistant_blocks));
    }
    sender
        .send_important(AgentEvent::TurnCompleted {
            final_text: final_text.clone(),
            messages: messages.clone(),
        })
        .await;
    Ok((final_text, messages.clone()))
}

async fn emit_assistant_completed_if_present(sender: &EventSender, text: &str) {
    if !text.is_empty() {
        sender
            .send_important(AgentEvent::AssistantCompleted {
                text: text.to_string(),
            })
            .await;
    }
}

async fn emit_malformed_tool_events(
    sender: &EventSender,
    malformed_tools: Vec<(String, String, ToolOutput)>,
) {
    for (id, name, error_output) in malformed_tools {
        sender
            .send_important(AgentEvent::ToolStarted {
                id: id.clone(),
                name,
            })
            .await;
        sender
            .send_important(AgentEvent::ToolCompleted {
                id,
                result: error_output,
            })
            .await;
    }
}

async fn finalize_tool_calls(
    turn: &mut AssistantTurnBuilder,
    sender: &EventSender,
) -> ToolTurnOutcome {
    let mut executable = Vec::with_capacity(turn.tool_uses.len());
    let mut assistant_tools = Vec::with_capacity(turn.tool_uses.len());
    let mut malformed_results = Vec::new();
    let mut malformed_tools = Vec::new();

    for tu in turn.tool_uses.drain(..) {
        match tu.clone().finalize() {
            Ok(tool_use) => {
                assistant_tools.push(tool_use.clone());
                executable.push(tool_use);
            }
            Err(e) => {
                sender
                    .send_important(AgentEvent::Error {
                        kind: ErrorKind::Parse,
                        message: format!("Invalid tool input JSON for {}: {}", tu.name, e),
                        details: Some(truncate_for_error(&tu.input_json, 500)),
                    })
                    .await;
                assistant_tools.push(ToolUse {
                    id: tu.id.clone(),
                    name: tu.name.clone(),
                    input: serde_json::json!({ "_raw_malformed": tu.input_json }),
                });
                let error_output = ToolOutput::failure(
                    "invalid_json",
                    format!("Failed to parse tool arguments: {e}"),
                    Some(truncate_for_error(&tu.input_json, 500)),
                );
                malformed_results.push(ToolResult::from_output(tu.id.clone(), &error_output));
                malformed_tools.push((tu.id, tu.name, error_output));
            }
        }
    }

    ToolTurnOutcome {
        executable,
        assistant_tools,
        malformed_results,
        malformed_tools,
    }
}

fn map_thinking_to_reasoning(level: ThinkingLevel) -> Option<String> {
    level.effort_label().map(std::string::ToString::to_string)
}

fn map_thinking_to_anthropic_effort(level: ThinkingLevel) -> Option<AnthropicEffortLevel> {
    match level {
        ThinkingLevel::Off => None,
        ThinkingLevel::Minimal | ThinkingLevel::Low => Some(AnthropicEffortLevel::Low),
        ThinkingLevel::Medium => Some(AnthropicEffortLevel::Medium),
        ThinkingLevel::High => Some(AnthropicEffortLevel::High),
        ThinkingLevel::XHigh => Some(AnthropicEffortLevel::Max),
    }
}

/// Executes all tool uses in parallel and emits events via async channel.
///
/// Tools are spawned concurrently using `tokio::JoinSet`. `ToolStarted` events
/// are emitted sequentially before spawning to preserve CLI output order.
/// `ToolCompleted` events are emitted as each task completes.
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
            () = interrupt::wait_for_interrupt() => {
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
                        eprintln!("Task join error: {e:?}");
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

    /// Verifies agent emits `ToolStarted` and `ToolCompleted` events (SPEC §7).
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

    /// Verifies `EventSender` `send_delta()` is best-effort (doesn't block on full channel).
    #[tokio::test]
    async fn test_event_sender_send_delta_is_best_effort() {
        // Create a tiny channel that will fill up quickly
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let sender = EventSender::new(tx);

        // This should not block even though channel is tiny
        for i in 0..100 {
            sender.send_delta(AgentEvent::AssistantDelta {
                text: format!("chunk {i}"),
            });
        }
        // If we got here without blocking, the test passes
    }

    /// Verifies `ToolUseBuilder` finalization fails on invalid JSON.
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
