//! Agent module for UI-agnostic execution.
//!
//! The agent drives the provider + tool loop and emits `AgentEvent`s
//! via async channels. No direct stdout/stderr writes occur in this module.

use std::collections::HashSet;
use std::future;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

use crate::config::{Config, TextVerbosity, ThinkingLevel};
use crate::core::events::{AgentEvent, ErrorKind, ToolOutput, TurnStatus};
use crate::core::interrupt::{self, InterruptedError};
use crate::providers::anthropic::{
    AnthropicClient, AnthropicConfig, ClaudeCliClient, ClaudeCliConfig,
    EffortLevel as AnthropicEffortLevel,
};
use crate::providers::apiyi::{ApiyiClient, ApiyiConfig};
use crate::providers::gemini::{
    GeminiCliClient, GeminiCliConfig, GeminiClient, GeminiConfig, GeminiThinkingConfig,
};
use crate::providers::minimax::{MinimaxClient, MinimaxConfig};
use crate::providers::mistral::{MistralClient, MistralConfig};
use crate::providers::moonshot::{MoonshotClient, MoonshotConfig};
use crate::providers::openai::{OpenAIClient, OpenAICodexClient, OpenAICodexConfig, OpenAIConfig};
use crate::providers::openrouter::{OpenRouterClient, OpenRouterConfig};
use crate::providers::stepfun::{StepfunClient, StepfunConfig};
use crate::providers::xai::{XaiClient, XaiConfig};
use crate::providers::xiaomi::{XiomiClient, XiomiConfig};
use crate::providers::zai::{ZaiClient, ZaiConfig};
use crate::providers::zen::{ZenClient, ZenConfig};
use crate::providers::{
    ChatContentBlock, ChatMessage, ContentBlockType, ProviderError, ProviderKind, ProviderStream,
    ReasoningBlock, ReplayToken, StreamEvent, resolve_provider,
};
use crate::subagents;
use crate::tools::{ToolContext, ToolDefinition, ToolRegistry, ToolResult, ToolSet, todo_write};

/// Options for agent execution.
#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// Root directory for file operations.
    pub root: PathBuf,
    /// Tool configuration (registry + selection).
    pub tool_config: ToolConfig,
    /// Surface label for activity tracking (e.g., "chat", "exec", "telegram").
    pub surface: Option<String>,
    /// Optional `OpenAI` Responses text verbosity override for this run.
    pub text_verbosity: Option<TextVerbosity>,
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

/// Channel-based event sender (unbounded).
///
/// Used with `run_turn` for concurrent rendering and thread persistence.
/// Events are wrapped in `Arc` for efficient cloning to multiple consumers.
pub type AgentEventTx = mpsc::UnboundedSender<Arc<AgentEvent>>;

/// Channel-based event receiver (unbounded).
pub type AgentEventRx = mpsc::UnboundedReceiver<Arc<AgentEvent>>;

/// Creates an unbounded event channel.
pub fn create_event_channel() -> (AgentEventTx, AgentEventRx) {
    mpsc::unbounded_channel()
}

/// Event sender wrapper with a single reliable send operation.
#[derive(Clone)]
pub struct EventSender {
    tx: AgentEventTx,
}

impl EventSender {
    /// Creates a new `EventSender` wrapping the given channel sender.
    pub fn new(tx: AgentEventTx) -> Self {
        Self { tx }
    }

    /// Sends an event. Never blocks; ignored silently if the receiver has dropped.
    pub fn send(&self, ev: AgentEvent) {
        let _ = self.tx.send(Arc::new(ev));
    }
}

enum ProviderClient {
    Anthropic(AnthropicClient),
    ClaudeCli(ClaudeCliClient),
    OpenAICodex(OpenAICodexClient),
    OpenAI(OpenAIClient),
    OpenRouter(OpenRouterClient),
    Xiomi(XiomiClient),
    Mistral(MistralClient),
    Moonshot(MoonshotClient),
    Stepfun(StepfunClient),
    Gemini(GeminiClient),
    GeminiCli(GeminiCliClient),
    Zen(ZenClient),
    Apiyi(ApiyiClient),
    Minimax(MinimaxClient),
    Zai(ZaiClient),
    Xai(XaiClient),
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
            ProviderClient::Xiomi(client) => {
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
            ProviderClient::Minimax(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::Zai(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
            ProviderClient::Xai(client) => {
                client.send_messages_stream(messages, tools, system).await
            }
        }
    }
}

/// Spawns a broadcast task that distributes events to multiple consumers.
///
/// Each event is cloned (cheaply, via `Arc`) and sent to all subscribers.
/// Closed channels are automatically removed.
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
pub fn spawn_broadcaster(rx: AgentEventRx, subscribers: Vec<AgentEventTx>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = rx;
        let mut subscribers = subscribers;
        while let Some(event) = rx.recv().await {
            subscribers.retain(|tx| tx.send(Arc::clone(&event)).is_ok());
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
    pub signature_provider: Option<crate::providers::SignatureProvider>,
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
        "edit" => "new_string",
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

type TurnResult<T> = std::result::Result<T, TurnError>;

#[derive(Debug, Clone)]
struct CompletedTurn {
    final_text: String,
    messages: Vec<ChatMessage>,
}

#[derive(Debug)]
enum TurnError {
    Interrupted {
        partial_content: Option<String>,
        completed_turn: Option<CompletedTurn>,
    },
    Provider(ProviderError),
    Parse {
        message: String,
        details: Option<String>,
    },
    Internal(anyhow::Error),
}

#[derive(Debug, Clone)]
enum TurnDiagnostic {
    Parse {
        message: String,
        details: Option<String>,
    },
}

impl TurnError {
    fn interrupted(partial_content: Option<String>) -> Self {
        Self::Interrupted {
            partial_content,
            completed_turn: None,
        }
    }

    fn interrupted_with_completion(
        partial_content: Option<String>,
        final_text: String,
        messages: Vec<ChatMessage>,
    ) -> Self {
        Self::Interrupted {
            partial_content,
            completed_turn: Some(CompletedTurn {
                final_text,
                messages,
            }),
        }
    }

    fn from_anyhow(err: anyhow::Error) -> Self {
        match err.downcast::<ProviderError>() {
            Ok(provider_err) => Self::Provider(provider_err),
            Err(err) => Self::Internal(err),
        }
    }

    fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Interrupted { .. } => InterruptedError.into(),
            Self::Provider(provider_err) => anyhow::Error::new(provider_err),
            Self::Parse { message, .. } => anyhow!(message),
            Self::Internal(err) => err,
        }
    }
}

fn emit_turn_error(err: &TurnError, sender: &EventSender) {
    match err {
        TurnError::Interrupted {
            partial_content,
            completed_turn,
        } => {
            let final_text = completed_turn
                .as_ref()
                .map(|turn| turn.final_text.clone())
                .or_else(|| partial_content.clone())
                .unwrap_or_default();
            let messages = completed_turn
                .as_ref()
                .map(|turn| turn.messages.clone())
                .unwrap_or_default();
            sender.send(AgentEvent::TurnFinished {
                status: TurnStatus::Interrupted,
                final_text,
                messages,
            });
        }
        TurnError::Provider(provider_err) => {
            sender.send(AgentEvent::TurnFinished {
                status: TurnStatus::Failed {
                    kind: provider_err.kind.clone().into(),
                    message: provider_err.message.clone(),
                    details: provider_err.details.clone(),
                },
                final_text: String::new(),
                messages: Vec::new(),
            });
        }
        TurnError::Parse { message, details } => {
            sender.send(AgentEvent::TurnFinished {
                status: TurnStatus::Failed {
                    kind: ErrorKind::Parse,
                    message: message.clone(),
                    details: details.clone(),
                },
                final_text: String::new(),
                messages: Vec::new(),
            });
        }
        TurnError::Internal(err) => {
            sender.send(AgentEvent::TurnFinished {
                status: TurnStatus::Failed {
                    kind: ErrorKind::Internal,
                    message: err.to_string(),
                    details: None,
                },
                final_text: String::new(),
                messages: Vec::new(),
            });
        }
    }
}

fn emit_turn_error_with_messages(err: &TurnError, messages: &[ChatMessage], sender: &EventSender) {
    match err {
        TurnError::Provider(provider_err) => {
            sender.send(AgentEvent::TurnFinished {
                status: TurnStatus::Failed {
                    kind: provider_err.kind.clone().into(),
                    message: provider_err.message.clone(),
                    details: provider_err.details.clone(),
                },
                final_text: String::new(),
                messages: messages.to_vec(),
            });
        }
        other => {
            // For non-Provider errors, fall back to default behavior (no messages)
            emit_turn_error(other, sender);
        }
    }
}

fn emit_turn_diagnostics(diagnostics: &[TurnDiagnostic], sender: &EventSender) {
    for diagnostic in diagnostics {
        match diagnostic {
            TurnDiagnostic::Parse { message, details } => {
                sender.send(AgentEvent::Error {
                    kind: ErrorKind::Parse,
                    message: message.clone(),
                    details: details.clone(),
                });
            }
        }
    }
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
/// Maximum number of automatic retries for transient provider errors.
const MAX_RETRIES: u32 = 3;
/// Base delay for exponential backoff (milliseconds).
const RETRY_BASE_DELAY_MS: u64 = 2000;

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
    run_turn_with_cancel(
        messages,
        config,
        options,
        system_prompt,
        thread_id,
        tx,
        None,
    )
    .await
}

/// # Errors
/// Returns an error if the operation fails.
pub async fn run_turn_with_cancel(
    messages: Vec<ChatMessage>,
    config: &Config,
    options: &AgentOptions,
    system_prompt: Option<&str>,
    thread_id: Option<&str>,
    tx: AgentEventTx,
    cancel: Option<CancellationToken>,
) -> Result<(String, Vec<ChatMessage>)> {
    let _run_guard = crate::agent_activity::start(thread_id, options.surface.as_deref());
    let sender = EventSender::new(tx);
    match run_turn_inner(
        messages,
        config,
        options,
        system_prompt,
        thread_id,
        &sender,
        cancel.as_ref(),
    )
    .await
    {
        Ok(result) => Ok(result),
        Err((err, committed_messages)) => {
            if committed_messages.is_empty() {
                emit_turn_error(&err, &sender);
            } else {
                emit_turn_error_with_messages(&err, &committed_messages, &sender);
            }
            Err(err.into_anyhow())
        }
    }
}

/// Result type for `run_turn_inner`: on error, includes committed messages for context preservation.
type RunTurnResult = std::result::Result<(String, Vec<ChatMessage>), (TurnError, Vec<ChatMessage>)>;

#[allow(clippy::too_many_lines)]
async fn run_turn_inner(
    messages: Vec<ChatMessage>,
    config: &Config,
    options: &AgentOptions,
    system_prompt: Option<&str>,
    thread_id: Option<&str>,
    sender: &EventSender,
    cancel: Option<&CancellationToken>,
) -> RunTurnResult {
    let setup = build_run_turn_setup(config, options, thread_id)
        .map_err(|e| (TurnError::from_anyhow(e), messages.clone()))?;
    let mut messages = messages;
    let mut consecutive_malformed_tool_turns = 0usize;

    loop {
        ensure_not_interrupted(None, cancel).map_err(|e| (e, messages.clone()))?;

        // Retry loop for pre-stream transient errors (connection, overloaded before SSE starts).
        // Mid-stream errors (from consume_stream) are NOT retried to avoid UI rewind complexity.
        let stream = match request_stream(
            &setup.client,
            &messages,
            &setup.tools,
            system_prompt,
            cancel,
        )
        .await
        {
            Ok(stream) => stream,
            Err(TurnError::Provider(ref initial_err)) if initial_err.is_retryable() => {
                // Track the most recent provider error across attempts so
                // each emitted ProviderRetry event reflects the *current*
                // failure kind/message/details, not the one from attempt 0.
                let mut current_err: ProviderError = initial_err.clone();
                let mut attempt: u32 = 0;
                loop {
                    attempt += 1;
                    let delay = RETRY_BASE_DELAY_MS * 2u64.pow(attempt - 1);
                    tracing::warn!(
                        attempt,
                        max = MAX_RETRIES,
                        delay_ms = delay,
                        error = %current_err.message,
                        "Transient provider error, retrying"
                    );
                    // Surface the retry in the transcript so TUI/bot users
                    // can see that we're backing off instead of staring at
                    // a frozen spinner. Non-fatal: the turn continues.
                    sender.send(AgentEvent::ProviderRetry {
                        kind: ErrorKind::from(current_err.kind.clone()),
                        message: current_err.message.clone(),
                        details: current_err.details.clone(),
                        attempt,
                        max_retries: MAX_RETRIES,
                        delay_ms: delay,
                    });
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    ensure_not_interrupted(None, cancel).map_err(|e| (e, messages.clone()))?;
                    match request_stream(
                        &setup.client,
                        &messages,
                        &setup.tools,
                        system_prompt,
                        cancel,
                    )
                    .await
                    {
                        Ok(s) => break s,
                        Err(retry_err) => {
                            if let TurnError::Provider(ref perr) = retry_err
                                && perr.is_retryable()
                                && attempt < MAX_RETRIES
                            {
                                current_err = perr.clone();
                                continue;
                            }
                            return Err((retry_err, messages.clone()));
                        }
                    }
                }
            }
            Err(err) => return Err((err, messages.clone())),
        };
        let mut stream_state = consume_stream(stream, &messages, sender, cancel)
            .await
            .map_err(|e| (e, messages.clone()))?;

        if stream_state.needs_tool_execution() {
            let stats = process_tool_turn(
                &mut messages,
                &mut stream_state.turn,
                &setup,
                sender,
                cancel,
            )
            .await
            .map_err(|e| (e, messages.clone()))?;
            if stats.executable == 0 && stats.malformed > 0 {
                consecutive_malformed_tool_turns += 1;
                if consecutive_malformed_tool_turns >= MAX_CONSECUTIVE_MALFORMED_TOOL_TURNS {
                    return Err((
                        TurnError::Parse {
                            message: MALFORMED_TOOL_LOOP_ABORT_MESSAGE.to_string(),
                            details: Some(
                                "Provider repeatedly requested tool calls without valid arguments"
                                    .to_string(),
                            ),
                        },
                        messages.clone(),
                    ));
                }
            } else {
                consecutive_malformed_tool_turns = 0;
            }
            continue;
        }

        return Ok(finalize_non_tool_turn(
            &mut messages,
            stream_state.turn,
            sender,
        ));
    }
}

struct RunTurnSetup {
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
        ProviderBuildOptions {
            text_verbosity: options.text_verbosity,
            thread_id,
            model: &selection.model,
            provider,
            max_tokens,
            thinking_level,
            model_output_limit,
        },
    )?;
    let tool_ctx = ToolContext::new(
        options.root.canonicalize().unwrap_or(options.root.clone()),
        config.tool_timeout(),
    )
    .with_current_thread_id(thread_id)
    .with_config(config);
    let tool_registry = options.tool_config.registry.clone();
    let tools = resolve_tools(config, options, provider, &tool_registry);
    let enabled_tools = tools.iter().map(|t| t.name.clone()).collect();

    Ok(RunTurnSetup {
        client,
        tools,
        enabled_tools,
        tool_ctx,
        tool_registry,
    })
}

#[derive(Clone, Copy)]
struct ProviderBuildOptions<'a> {
    text_verbosity: Option<TextVerbosity>,
    thread_id: Option<&'a str>,
    model: &'a str,
    provider: ProviderKind,
    max_tokens: u32,
    thinking_level: ThinkingLevel,
    model_output_limit: Option<u32>,
}

#[allow(clippy::too_many_lines)]
fn build_provider_client(
    config: &Config,
    options: ProviderBuildOptions<'_>,
) -> Result<ProviderClient> {
    let cache_key = options.thread_id.map(std::string::ToString::to_string);
    let thinking_enabled = options.thinking_level.is_enabled();
    let reasoning_effort = map_thinking_to_reasoning(options.thinking_level);
    let thinking_budget_tokens = options
        .thinking_level
        .compute_reasoning_budget(options.max_tokens, options.model_output_limit)
        .unwrap_or(0);
    let thinking_effort = map_thinking_to_anthropic_effort(options.thinking_level);
    let gemini_thinking = thinking_enabled
        .then(|| GeminiThinkingConfig::from_thinking_level(options.thinking_level, options.model));

    match options.provider {
        ProviderKind::Anthropic => build_anthropic_client(
            config,
            options.model,
            options.max_tokens,
            thinking_enabled,
            thinking_budget_tokens,
            thinking_effort,
        ),
        ProviderKind::ClaudeCli => Ok(ProviderClient::ClaudeCli(ClaudeCliClient::new(
            ClaudeCliConfig::new(
                options.model.to_string(),
                options.max_tokens,
                config.providers.claude_cli.effective_base_url(),
                thinking_enabled,
                thinking_budget_tokens,
                thinking_effort,
            ),
        ))),
        ProviderKind::OpenAICodex => Ok(ProviderClient::OpenAICodex(OpenAICodexClient::new(
            OpenAICodexConfig::new(
                options.model.to_string(),
                options.max_tokens,
                reasoning_effort,
                resolve_text_verbosity(
                    options.text_verbosity,
                    config.providers.openai_codex.effective_text_verbosity(),
                ),
                cache_key,
            ),
        ))),
        ProviderKind::OpenAI => build_openai_client(
            config,
            options.model,
            config.max_tokens,
            reasoning_effort,
            options.text_verbosity,
            cache_key,
        ),
        ProviderKind::OpenRouter => {
            build_openrouter_client(config, options.model, reasoning_effort, cache_key)
        }
        ProviderKind::Xiomi => build_xiaomi_client(config, options.model, thinking_enabled),
        ProviderKind::Mistral => {
            build_mistral_client(config, options.model, cache_key, thinking_enabled)
        }
        ProviderKind::Moonshot => {
            build_moonshot_client(config, options.model, cache_key, thinking_enabled)
        }
        ProviderKind::Stepfun => {
            build_stepfun_client(config, options.model, cache_key, thinking_enabled)
        }
        ProviderKind::Minimax => {
            build_minimax_client(config, options.model, cache_key, thinking_enabled)
        }
        ProviderKind::Zai => build_zai_client(config, options.model, cache_key, thinking_enabled),
        ProviderKind::Xai => build_xai_client(config, options.model, cache_key, thinking_enabled),
        ProviderKind::Gemini => {
            build_gemini_client(config, options.model, config.max_tokens, gemini_thinking)
        }
        ProviderKind::GeminiCli => Ok(ProviderClient::GeminiCli(GeminiCliClient::new(
            GeminiCliConfig::new(
                options.model.to_string(),
                config.max_tokens,
                gemini_thinking,
            ),
        ))),
        ProviderKind::Zen => build_zen_client(
            config,
            options.model,
            config.max_tokens,
            options.max_tokens,
            thinking_enabled,
            thinking_budget_tokens,
            thinking_effort,
            gemini_thinking.clone(),
            reasoning_effort,
            cache_key,
        ),
        ProviderKind::Apiyi => build_apiyi_client(
            config,
            options.model,
            config.max_tokens,
            options.max_tokens,
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
    max_tokens: Option<u32>,
    reasoning_effort: Option<String>,
    text_verbosity: Option<TextVerbosity>,
    cache_key: Option<String>,
) -> Result<ProviderClient> {
    Ok(ProviderClient::OpenAI(OpenAIClient::new(
        OpenAIConfig::from_env(
            model.to_string(),
            max_tokens,
            config.providers.openai.effective_base_url(),
            config.providers.openai.effective_api_key(),
            reasoning_effort,
            resolve_text_verbosity(
                text_verbosity,
                config.providers.openai.effective_text_verbosity(),
            ),
            cache_key,
        )?,
    )))
}

fn resolve_text_verbosity(
    runtime_override: Option<TextVerbosity>,
    provider_default: Option<TextVerbosity>,
) -> Option<TextVerbosity> {
    runtime_override.or(provider_default)
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

fn build_xiaomi_client(
    config: &Config,
    model: &str,
    thinking_enabled: bool,
) -> Result<ProviderClient> {
    Ok(ProviderClient::Xiomi(XiomiClient::new(
        XiomiConfig::from_env(
            model.to_string(),
            config.max_tokens,
            config.providers.xiaomi.effective_base_url(),
            config.providers.xiaomi.effective_api_key(),
            None,
            thinking_enabled,
        )?,
    )))
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

fn build_minimax_client(
    config: &Config,
    model: &str,
    cache_key: Option<String>,
    thinking_enabled: bool,
) -> Result<ProviderClient> {
    Ok(ProviderClient::Minimax(MinimaxClient::new(
        MinimaxConfig::from_env(
            model.to_string(),
            config.max_tokens,
            config.providers.minimax.effective_base_url(),
            config.providers.minimax.effective_api_key(),
            cache_key,
            thinking_enabled,
        )?,
    )))
}

fn build_zai_client(
    config: &Config,
    model: &str,
    cache_key: Option<String>,
    thinking_enabled: bool,
) -> Result<ProviderClient> {
    Ok(ProviderClient::Zai(ZaiClient::new(ZaiConfig::from_env(
        model.to_string(),
        config.max_tokens,
        config.providers.zai.effective_base_url(),
        config.providers.zai.effective_api_key(),
        cache_key,
        thinking_enabled,
    )?)))
}

fn build_xai_client(
    config: &Config,
    model: &str,
    cache_key: Option<String>,
    thinking_enabled: bool,
) -> Result<ProviderClient> {
    Ok(ProviderClient::Xai(XaiClient::new(XaiConfig::from_env(
        model.to_string(),
        config.max_tokens,
        config.providers.xai.effective_base_url(),
        config.providers.xai.effective_api_key(),
        cache_key,
        thinking_enabled,
    )?)))
}

fn build_gemini_client(
    config: &Config,
    model: &str,
    max_tokens: Option<u32>,
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
    max_tokens: Option<u32>,
    fallback_max_tokens: u32,
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
        fallback_max_tokens,
        config.providers.zen.effective_base_url(),
        config.providers.zen.effective_api_key(),
        thinking_enabled,
        thinking_budget_tokens,
        thinking_effort,
        gemini_thinking,
        reasoning_effort,
        cache_key,
        crate::models::ModelOption::find_by_provider_and_id("zen", model)
            .and_then(|m| m.capabilities.api)
            .map(ToString::to_string),
    )?)))
}

#[allow(clippy::too_many_arguments)]
fn build_apiyi_client(
    config: &Config,
    model: &str,
    max_tokens: Option<u32>,
    fallback_max_tokens: u32,
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
            fallback_max_tokens,
            config.providers.apiyi.effective_base_url(),
            config.providers.apiyi.effective_api_key(),
            thinking_enabled,
            thinking_budget_tokens,
            thinking_effort,
            gemini_thinking,
            reasoning_effort,
            cache_key,
            crate::models::ModelOption::find_by_provider_and_id("apiyi", model)
                .and_then(|m| m.capabilities.api)
                .map(ToString::to_string),
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

    if config.subagents.enabled {
        match subagents::list_summaries(&options.root) {
            Ok(available_subagents) => {
                for tool in &mut tools {
                    if tool.name.eq_ignore_ascii_case("Invoke_Subagent") {
                        *tool =
                            crate::tools::subagent::definition_with_subagents(&available_subagents);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "Failed to discover subagents for Invoke_Subagent tool description"
                );
            }
        }
    } else {
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

fn ensure_not_interrupted(
    partial_content: Option<String>,
    cancel: Option<&CancellationToken>,
) -> TurnResult<()> {
    if interrupt::is_interrupted() || cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(TurnError::interrupted(partial_content));
    }
    Ok(())
}

async fn request_stream(
    client: &ProviderClient,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    system_prompt: Option<&str>,
    cancel: Option<&CancellationToken>,
) -> TurnResult<ProviderStream> {
    let stream_result = tokio::select! {
        biased;
        () = interrupt::wait_for_interrupt() => {
            return Err(TurnError::interrupted(None));
        }
        () = wait_for_cancel(cancel) => {
            return Err(TurnError::interrupted(None));
        }
        result = client.send_messages_stream(messages, tools, system_prompt) => result,
    };
    match stream_result {
        Ok(stream) => Ok(stream),
        Err(err) => Err(TurnError::from_anyhow(err)),
    }
}

async fn consume_stream(
    mut stream: ProviderStream,
    prior_messages: &[ChatMessage],
    sender: &EventSender,
    cancel: Option<&CancellationToken>,
) -> TurnResult<StreamState> {
    let mut state = StreamState::new();

    loop {
        if interrupt::is_interrupted() || cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(interrupted_turn_from_stream(prior_messages, state.turn));
        }
        let event = match timeout(STREAM_POLL_TIMEOUT, stream.next()).await {
            Ok(Some(result)) => result.map_err(TurnError::Provider)?,
            Ok(None) => return Ok(state),
            Err(_) => continue,
        };
        handle_stream_event(event, sender, &mut state)?;
    }
}

fn handle_stream_event(
    event: StreamEvent,
    sender: &EventSender,
    state: &mut StreamState,
) -> TurnResult<()> {
    match event {
        StreamEvent::TextDelta { text, .. } if !text.is_empty() => {
            state.turn.text.push_str(&text);
            sender.send(AgentEvent::AssistantDelta { text });
        }
        StreamEvent::ContentBlockStart {
            index,
            block_type: ContentBlockType::ToolUse,
            id,
            name,
        } => {
            handle_tool_content_start(index, id, name, sender, &mut state.turn);
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
                signature_provider: None,
                replay: None,
                had_delta: false,
            });
        }
        StreamEvent::InputJsonDelta {
            index,
            partial_json,
        } => handle_input_json_delta(index, &partial_json, sender, &mut state.turn),
        StreamEvent::ReasoningDelta { index, reasoning } => {
            if let Some(tb) = state.turn.find_thinking_mut(index) {
                if !reasoning.is_empty() {
                    tb.had_delta = true;
                }
                tb.text.push_str(&reasoning);
                sender.send(AgentEvent::ReasoningDelta { text: reasoning });
            }
        }
        StreamEvent::ReasoningSignatureDelta {
            index,
            signature,
            provider,
        } => {
            if let Some(tb) = state.turn.find_thinking_mut(index) {
                tb.signature.push_str(&signature);
                tb.signature_provider = Some(provider);
            }
        }
        StreamEvent::ContentBlockCompleted { index } => {
            emit_reasoning_completion(sender, &mut state.turn, index);
            emit_tool_input_completion(sender, &state.turn, index);
        }
        StreamEvent::MessageDelta { stop_reason, usage } => {
            state.stop_reason = stop_reason;
            emit_message_delta_usage(sender, usage);
        }
        StreamEvent::Error {
            error_type,
            message,
        } => {
            return Err(TurnError::Provider(ProviderError::api_error(
                &error_type,
                &message,
            )));
        }
        StreamEvent::MessageStart { usage, .. } => emit_message_start_usage(sender, &usage),
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

fn handle_tool_content_start(
    index: usize,
    id: Option<String>,
    name: Option<String>,
    sender: &EventSender,
    turn: &mut AssistantTurnBuilder,
) {
    let tool_id = id.unwrap_or_default();
    let tool_name = name.unwrap_or_default().to_ascii_lowercase();
    sender.send(AgentEvent::ToolRequested {
        id: tool_id.clone(),
        name: tool_name.clone(),
        input: serde_json::json!({}),
    });
    turn.tool_uses.push(ToolUseBuilder {
        index,
        id: tool_id,
        name: tool_name,
        input_json: String::new(),
        input_preview_len: 0,
    });
}

fn handle_input_json_delta(
    index: usize,
    partial_json: &str,
    sender: &EventSender,
    turn: &mut AssistantTurnBuilder,
) {
    if let Some(tu) = turn.find_tool_use_mut(index) {
        tu.input_json.push_str(partial_json);
        if let Some(delta) = extract_partial_tool_input(&tu.name, &tu.input_json)
            && !delta.is_empty()
            && delta.len() > tu.input_preview_len
        {
            tu.input_preview_len = delta.len();
            sender.send(AgentEvent::ToolInputDelta {
                id: tu.id.clone(),
                name: tu.name.clone(),
                delta,
            });
        }
    }
}

fn emit_message_delta_usage(sender: &EventSender, usage: Option<crate::providers::Usage>) {
    if let Some(u) = usage {
        sender.send(AgentEvent::UsageUpdate {
            input_tokens: 0,
            output_tokens: u.output_tokens,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        });
    }
}

fn emit_message_start_usage(sender: &EventSender, usage: &crate::providers::Usage) {
    sender.send(AgentEvent::UsageUpdate {
        input_tokens: usage.input_tokens,
        output_tokens: 0,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
    });
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

fn emit_reasoning_completion(sender: &EventSender, turn: &mut AssistantTurnBuilder, index: usize) {
    if let Some(tb) = turn.find_thinking_mut(index) {
        if tb.replay.is_none()
            && !tb.signature.is_empty()
            && let Some(signature_provider) = tb.signature_provider
        {
            tb.replay = Some(match signature_provider {
                crate::providers::SignatureProvider::Gemini => ReplayToken::Gemini {
                    signature: tb.signature.clone(),
                },
                crate::providers::SignatureProvider::Anthropic => ReplayToken::Anthropic {
                    signature: tb.signature.clone(),
                },
            });
        }
        let block = ReasoningBlock {
            text: (!tb.text.is_empty()).then(|| tb.text.clone()),
            replay: tb.replay.clone(),
        };
        sender.send(AgentEvent::ReasoningCompleted { block });
    }
}

fn emit_tool_input_completion(sender: &EventSender, turn: &AssistantTurnBuilder, index: usize) {
    if let Some(tu) = turn.tool_uses.iter().find(|t| t.index == index) {
        let input: Value =
            serde_json::from_str(&tu.input_json).unwrap_or_else(|_| serde_json::json!({}));
        sender.send(AgentEvent::ToolInputCompleted {
            id: tu.id.clone(),
            name: tu.name.clone(),
            input,
        });
    }
}

struct ToolTurnOutcome {
    executable: Vec<ToolUse>,
    assistant_tools: Vec<ToolUse>,
    malformed_results: Vec<ToolResult>,
    malformed_tools: Vec<(String, String, ToolOutput)>,
    diagnostics: Vec<TurnDiagnostic>,
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
    cancel: Option<&CancellationToken>,
) -> TurnResult<ToolTurnStats> {
    let outcome = finalize_tool_calls(turn);
    let executable_count = outcome.executable.len();
    let malformed_count = outcome.malformed_results.len();
    emit_assistant_completed_if_present(sender, &turn.text);
    emit_turn_diagnostics(&outcome.diagnostics, sender);
    emit_malformed_tool_events(sender, outcome.malformed_tools);

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
        cancel,
    )
    .await;
    tool_results.extend(outcome.malformed_results);
    messages.push(ChatMessage::tool_results(tool_results));

    if interrupt::is_interrupted() || cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(TurnError::interrupted_with_completion(
            (!turn_text.is_empty()).then_some(turn_text.clone()),
            turn_text,
            messages.clone(),
        ));
    }

    Ok(ToolTurnStats {
        executable: executable_count,
        malformed: malformed_count,
    })
}

fn finalize_non_tool_turn(
    messages: &mut Vec<ChatMessage>,
    turn: AssistantTurnBuilder,
    sender: &EventSender,
) -> (String, Vec<ChatMessage>) {
    let final_text = turn.text.clone();
    emit_assistant_completed_if_present(sender, &final_text);
    let assistant_blocks = turn.into_blocks(Vec::new());
    if !assistant_blocks.is_empty() {
        messages.push(ChatMessage::assistant_blocks(assistant_blocks));
    }
    sender.send(AgentEvent::TurnFinished {
        status: TurnStatus::Completed,
        final_text: final_text.clone(),
        messages: messages.clone(),
    });
    (final_text, messages.clone())
}

fn interrupted_turn_from_stream(
    prior_messages: &[ChatMessage],
    turn: AssistantTurnBuilder,
) -> TurnError {
    let final_text = turn.text.clone();
    let messages = build_interrupted_messages(prior_messages, turn);
    TurnError::interrupted_with_completion(
        (!final_text.is_empty()).then_some(final_text.clone()),
        final_text,
        messages,
    )
}

fn build_interrupted_messages(
    prior_messages: &[ChatMessage],
    mut turn: AssistantTurnBuilder,
) -> Vec<ChatMessage> {
    let mut assistant_tools = Vec::with_capacity(turn.tool_uses.len());
    let mut tool_results = Vec::with_capacity(turn.tool_uses.len());

    for tu in turn.tool_uses.drain(..) {
        match tu.clone().finalize() {
            Ok(tool_use) => {
                let interrupted_output = ToolOutput::canceled("Interrupted by user");
                tool_results.push(ToolResult::from_output(
                    tool_use.id.clone(),
                    &interrupted_output,
                ));
                assistant_tools.push(tool_use);
            }
            Err(err) => {
                assistant_tools.push(ToolUse {
                    id: tu.id.clone(),
                    name: tu.name.clone(),
                    input: serde_json::json!({ "_raw_malformed": tu.input_json }),
                });
                let error_output = ToolOutput::failure(
                    "invalid_json",
                    format!("Failed to parse tool arguments: {err}"),
                    Some(truncate_for_error(&tu.input_json, 500)),
                );
                tool_results.push(ToolResult::from_output(tu.id, &error_output));
            }
        }
    }

    let assistant_blocks = turn.into_blocks(assistant_tools);
    let mut messages = prior_messages.to_vec();

    if !assistant_blocks.is_empty() {
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            phase: Some("commentary".to_string()),
            content: crate::providers::MessageContent::Blocks(assistant_blocks),
        });
    }

    if !tool_results.is_empty() {
        messages.push(ChatMessage::tool_results(tool_results));
    }

    messages
}

fn emit_assistant_completed_if_present(sender: &EventSender, text: &str) {
    if !text.is_empty() {
        sender.send(AgentEvent::AssistantCompleted {
            text: text.to_string(),
        });
    }
}

fn emit_malformed_tool_events(
    sender: &EventSender,
    malformed_tools: Vec<(String, String, ToolOutput)>,
) {
    for (id, name, error_output) in malformed_tools {
        sender.send(AgentEvent::ToolStarted {
            id: id.clone(),
            name,
        });
        sender.send(AgentEvent::ToolCompleted {
            id,
            result: error_output,
        });
    }
}

fn finalize_tool_calls(turn: &mut AssistantTurnBuilder) -> ToolTurnOutcome {
    let mut executable = Vec::with_capacity(turn.tool_uses.len());
    let mut assistant_tools = Vec::with_capacity(turn.tool_uses.len());
    let mut malformed_results = Vec::new();
    let mut malformed_tools = Vec::new();
    let mut diagnostics = Vec::new();

    for tu in turn.tool_uses.drain(..) {
        match tu.clone().finalize() {
            Ok(tool_use) => {
                assistant_tools.push(tool_use.clone());
                executable.push(tool_use);
            }
            Err(e) => {
                diagnostics.push(TurnDiagnostic::Parse {
                    message: format!("Invalid tool input JSON for {}: {}", tu.name, e),
                    details: Some(truncate_for_error(&tu.input_json, 500)),
                });
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
        diagnostics,
    }
}

fn map_thinking_to_reasoning(level: ThinkingLevel) -> Option<String> {
    match level {
        ThinkingLevel::Off => None,
        // OpenAI reasoning.effort doesn't support "minimal"; use the lowest
        // supported effort instead.
        ThinkingLevel::Minimal | ThinkingLevel::Low => Some("low".to_string()),
        ThinkingLevel::Medium => Some("medium".to_string()),
        ThinkingLevel::High => Some("high".to_string()),
        ThinkingLevel::XHigh => Some("xhigh".to_string()),
    }
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
    cancel: Option<&CancellationToken>,
) -> Vec<ToolResult> {
    let mut join_set: JoinSet<(usize, String, ToolOutput, ToolResult)> = JoinSet::new();
    let mut results: Vec<Option<(ToolOutput, ToolResult)>> = vec![None; tool_uses.len()];
    let mut completed: HashSet<usize> = HashSet::new();
    let mut current_todo_state: Option<todo_write::TodoState> = None;

    let is_enabled_tool = |name: &str| {
        let name_lower = name.to_ascii_lowercase();
        enabled_tools
            .iter()
            .any(|tool| tool.to_ascii_lowercase() == name_lower)
    };

    emit_tool_started_events(tool_uses, sender);

    // Execute todo_write calls in order so later calls in the same turn can see
    // earlier mutations without waiting for thread persistence. Spawn everything
    // else concurrently as before.
    for (i, tu) in tool_uses.iter().enumerate() {
        if todo_write::is_todo_tool_name(&tu.name) && is_enabled_tool(&tu.name) {
            let output = todo_write::execute_with_state(
                &tu.input,
                current_todo_state.as_ref(),
                ctx.current_thread_id.as_deref(),
            );
            if let Some(state) = todo_write::state_from_output(&output) {
                current_todo_state = Some(state);
            }
            let result = ToolResult::from_output(tu.id.clone(), &output);
            record_tool_completion(
                sender,
                &mut completed,
                &mut results,
                i,
                tu.id.clone(),
                output,
                result,
            );
            continue;
        }

        // Clone for 'static requirement
        let tu = tu.clone();
        let mut ctx = ctx.clone();
        ctx.event_sender = Some(sender.clone());
        ctx.tool_use_id = Some(tu.id.clone());
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
                handle_tool_interrupt(
                    &mut join_set,
                    &mut completed,
                    &mut results,
                    tool_uses,
                    sender,
                );
                break;
            }
            () = wait_for_cancel(cancel) => {
                handle_tool_interrupt(
                    &mut join_set,
                    &mut completed,
                    &mut results,
                    tool_uses,
                    sender,
                );
                break;
            }
            task_result = join_set.join_next() => {
                match task_result {
                    Some(Ok((idx, id, output, result))) => {
                        record_tool_completion(
                            sender,
                            &mut completed,
                            &mut results,
                            idx,
                            id,
                            output,
                            result,
                        );
                    }
                    Some(Err(e)) => {
                        // JoinError: panic or cancellation
                        // This is rare and typically only happens if a task panics.
                        // Log it but continue - the slot will remain None and be
                        // caught by the expect below (which is a bug if it happens).
                        tracing::error!(err = ?e, "Task join error");
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

async fn wait_for_cancel(cancel: Option<&CancellationToken>) {
    if let Some(cancel) = cancel {
        cancel.cancelled().await;
    } else {
        future::pending::<()>().await;
    }
}

fn emit_tool_started_events(tool_uses: &[ToolUse], sender: &EventSender) {
    for tu in tool_uses {
        sender.send(AgentEvent::ToolStarted {
            id: tu.id.clone(),
            name: tu.name.clone(),
        });
    }
}

fn record_tool_completion(
    sender: &EventSender,
    completed: &mut HashSet<usize>,
    results: &mut [Option<(ToolOutput, ToolResult)>],
    idx: usize,
    id: String,
    output: ToolOutput,
    result: ToolResult,
) {
    completed.insert(idx);
    sender.send(AgentEvent::ToolCompleted {
        id,
        result: output.clone(),
    });
    results[idx] = Some((output, result));
}

fn handle_tool_interrupt(
    join_set: &mut JoinSet<(usize, String, ToolOutput, ToolResult)>,
    completed: &mut HashSet<usize>,
    results: &mut [Option<(ToolOutput, ToolResult)>],
    tool_uses: &[ToolUse],
    sender: &EventSender,
) {
    join_set.abort_all();

    while let Some(task_result) = join_set.try_join_next() {
        if let Ok((idx, id, output, result)) = task_result
            && !completed.contains(&idx)
        {
            record_tool_completion(sender, completed, results, idx, id, output, result);
        }
    }

    for (i, tu) in tool_uses.iter().enumerate() {
        if !completed.contains(&i) {
            let abort_output = ToolOutput::canceled("Interrupted by user");
            let abort_result = ToolResult::from_output(tu.id.clone(), &abort_output);
            record_tool_completion(
                sender,
                completed,
                results,
                i,
                tu.id.clone(),
                abort_output,
                abort_result,
            );
        }
    }
}

/// Builds assistant content blocks from accumulated thinking, reasoning, text, and tool uses.
#[cfg(test)]
mod tests {
    use tokio::time::{Duration, timeout};

    use super::*;

    #[test]
    fn openai_runtime_text_verbosity_overrides_provider_config() {
        assert_eq!(
            resolve_text_verbosity(Some(TextVerbosity::Low), Some(TextVerbosity::High)),
            Some(TextVerbosity::Low)
        );
    }

    #[test]
    fn openai_codex_runtime_text_verbosity_overrides_provider_config() {
        assert_eq!(
            resolve_text_verbosity(Some(TextVerbosity::Low), Some(TextVerbosity::High)),
            Some(TextVerbosity::Low)
        );
    }

    #[test]
    fn provider_text_verbosity_is_used_when_runtime_override_is_absent() {
        assert_eq!(
            resolve_text_verbosity(None, Some(TextVerbosity::High)),
            Some(TextVerbosity::High)
        );
    }

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
            input: serde_json::json!({"file_path": "test.txt"}),
        }];

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        // Run in a task so we can collect events
        let tool_registry = ToolRegistry::builtins();
        let handle = tokio::spawn(async move {
            execute_tools_async(
                &tool_uses,
                &ctx,
                &enabled_tools,
                &sender,
                &tool_registry,
                None,
            )
            .await
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

    #[tokio::test]
    async fn test_execute_tools_carries_todo_state_within_turn() {
        let ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        let enabled_tools: HashSet<String> = vec!["todo_write".to_string()].into_iter().collect();
        let tool_uses = vec![
            ToolUse {
                id: "tool1".to_string(),
                name: "todo_write".to_string(),
                input: serde_json::json!({
                    "ops": [
                        {"op": "add", "content": "Inspect codebase", "status": "in_progress"}
                    ]
                }),
            },
            ToolUse {
                id: "tool2".to_string(),
                name: "todo_write".to_string(),
                input: serde_json::json!({
                    "ops": [
                        {"op": "update", "id": "task-1", "status": "completed"},
                        {"op": "add", "content": "Ship fix"}
                    ]
                }),
            },
        ];

        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let tool_registry = ToolRegistry::builtins();

        let results = execute_tools_async(
            &tool_uses,
            &ctx,
            &enabled_tools,
            &sender,
            &tool_registry,
            None,
        )
        .await;

        assert_eq!(results.len(), 2);
        assert!(!results[0].is_error);
        assert!(!results[1].is_error);

        let second = results[1]
            .content
            .as_text()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .expect("todo_write result should be valid JSON text");

        let tasks = second
            .get("data")
            .and_then(|data| data.get("tasks"))
            .and_then(|tasks| tasks.as_array())
            .expect("todo_write should return tasks array");

        assert_eq!(tasks.len(), 2);
        assert_eq!(
            tasks[0].get("id").and_then(|id| id.as_str()),
            Some("task-1")
        );
        assert_eq!(
            tasks[0].get("status").and_then(|status| status.as_str()),
            Some("completed")
        );
        assert_eq!(
            tasks[1].get("id").and_then(|id| id.as_str()),
            Some("task-2")
        );
        assert_eq!(
            tasks[1].get("status").and_then(|status| status.as_str()),
            Some("in_progress")
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

    /// Verifies `EventSender::send` delivers events reliably via unbounded channel.
    #[tokio::test]
    async fn test_event_sender_send_delivers_all_events() {
        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        for i in 0..100 {
            sender.send(AgentEvent::AssistantDelta {
                text: format!("chunk {i}"),
            });
        }

        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 100, "all 100 events should be delivered");
    }

    /// Verifies provider failures are rendered as a failed terminal event.
    #[tokio::test]
    async fn test_emit_turn_error_provider_emits_failed_turn_finished() {
        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let err = TurnError::Provider(ProviderError::api_error("overloaded_error", "HTTP 502"));
        emit_turn_error(&err, &sender);

        let event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed unexpectedly");
        assert!(matches!(
            &*event,
            AgentEvent::TurnFinished {
                status: TurnStatus::Failed {
                    kind: ErrorKind::ApiError,
                    message,
                    details: _,
                },
                final_text,
                messages,
            } if message == "overloaded_error: HTTP 502"
                && final_text.is_empty()
                && messages.is_empty()
        ));
        assert!(rx.try_recv().is_err());
    }

    /// Verifies non-fatal diagnostics are emitted through the centralized helper.
    #[tokio::test]
    async fn test_emit_turn_diagnostics_parse_emits_error_event() {
        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let diagnostics = vec![TurnDiagnostic::Parse {
            message: "Invalid tool input JSON for read: expected value".to_string(),
            details: Some("{bad json}".to_string()),
        }];

        emit_turn_diagnostics(&diagnostics, &sender);

        let event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed unexpectedly");
        assert!(matches!(
            &*event,
            AgentEvent::Error {
                kind: ErrorKind::Parse,
                message,
                details
            } if message == "Invalid tool input JSON for read: expected value"
                && details.as_deref() == Some("{bad json}")
        ));
        assert!(rx.try_recv().is_err());
    }

    /// Verifies interrupted turns emit a single interrupted terminal event.
    #[tokio::test]
    async fn test_emit_turn_error_interrupted_emits_turn_finished() {
        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let messages = vec![ChatMessage::assistant_text("partial", None)];

        let err = TurnError::interrupted_with_completion(
            Some("partial".to_string()),
            "partial".to_string(),
            messages.clone(),
        );
        emit_turn_error(&err, &sender);

        let event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed unexpectedly");
        assert!(matches!(
            &*event,
            AgentEvent::TurnFinished {
                status: TurnStatus::Interrupted,
                final_text,
                messages: event_messages,
            } if final_text == "partial" && event_messages == &messages
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn interrupted_stream_snapshot_preserves_partial_context_for_next_turn() {
        use crate::providers::{ChatContentBlock, MessageContent};
        use crate::tools::ToolResultContent;

        let prior_messages = vec![ChatMessage::user("analyze the repo")];
        let turn = AssistantTurnBuilder {
            thinking_blocks: vec![ThinkingBuilder {
                index: 0,
                text: "Let me inspect the project first.".to_string(),
                signature: String::new(),
                signature_provider: None,
                replay: Some(ReplayToken::Anthropic {
                    signature: "sig123".to_string(),
                }),
                had_delta: true,
            }],
            text: "I found the provider loop.".to_string(),
            tool_uses: vec![ToolUseBuilder {
                index: 1,
                id: "tool_1".to_string(),
                name: "read".to_string(),
                input_json: r#"{"file_path":"src/main.rs"}"#.to_string(),
                input_preview_len: 0,
            }],
        };

        let messages = build_interrupted_messages(&prior_messages, turn);

        assert_eq!(messages.len(), 3, "messages: {messages:#?}");
        assert_eq!(messages[0], prior_messages[0]);

        assert!(matches!(
            &messages[1],
            ChatMessage {
                role,
                phase: Some(phase),
                content: MessageContent::Blocks(blocks),
            } if role == "assistant"
                && phase == "commentary"
                && blocks.iter().any(|block| matches!(
                    block,
                    ChatContentBlock::Reasoning(ReasoningBlock {
                        text: Some(text),
                        replay: Some(ReplayToken::Anthropic { signature }),
                    }) if text == "Let me inspect the project first." && signature == "sig123"
                ))
                && blocks.iter().any(|block| matches!(
                    block,
                    ChatContentBlock::Text(text) if text == "I found the provider loop."
                ))
                && blocks.iter().any(|block| matches!(
                    block,
                    ChatContentBlock::ToolUse { id, name, input }
                        if id == "tool_1"
                            && name == "read"
                            && input == &serde_json::json!({"file_path": "src/main.rs"})
                ))
        ));

        assert!(matches!(
            &messages[2],
            ChatMessage {
                role,
                content: MessageContent::Blocks(blocks),
                ..
            } if role == "user"
                && matches!(blocks.as_slice(), [ChatContentBlock::ToolResult(result)]
                    if result.tool_use_id == "tool_1"
                        && result.is_error
                        && matches!(
                            &result.content,
                            ToolResultContent::Text(text)
                                if text.contains("\"code\":\"canceled\"")
                                    && text.contains("Interrupted by user")
                        ))
        ));
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
            .unwrap();

        // out1 should receive it
        let ev = timeout(Duration::from_secs(1), out1_rx.recv())
            .await
            .expect("timeout")
            .expect("should receive event");
        assert!(matches!(&*ev, AgentEvent::AssistantDelta { text } if text == "test"));
    }
}
