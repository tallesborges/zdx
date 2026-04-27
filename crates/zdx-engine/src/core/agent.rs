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
use crate::core::events::{AgentEvent, ErrorKind, NoticeKind, ToolOutput, TurnStatus};
use crate::core::interrupt::{self, InterruptedError};
use crate::providers::anthropic::{
    AnthropicClient, AnthropicConfig, ClaudeCliClient, ClaudeCliConfig,
    EffortLevel as AnthropicEffortLevel,
};
use crate::providers::apiyi::{ApiyiClient, ApiyiConfig};
use crate::providers::deepseek::{DeepSeekClient, DeepSeekConfig};
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
    /// `OpenAI` Responses API service tier: `"priority"` for faster inference (2× cost).
    pub service_tier: Option<String>,
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
    DeepSeek(DeepSeekClient),
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
            ProviderClient::DeepSeek(client) => {
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
    /// Whether the provider emitted `id` (`Real`) or the SSE parser
    /// synthesized one (`Synthesized`). Threaded into the resulting
    /// `ChatContentBlock::ToolUse` so the request builder can decide whether
    /// to replay the id on the next turn.
    pub id_origin: zdx_types::IdOrigin,
    /// Per-part replay metadata (e.g. Gemini `thoughtSignature`) captured
    /// when the stream emits a `ContentBlockCompleted` for this index.
    pub replay: Option<ReplayToken>,
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

/// Builder for accumulating a single text part's content + per-part replay
/// metadata. Each Gemini text part with its own `thoughtSignature` becomes
/// one of these, preserving the per-part fidelity required for implicit
/// prompt-cache hits.
#[derive(Debug, Clone)]
pub struct TextPartBuilder {
    pub index: usize,
    pub text: String,
    pub replay: Option<ReplayToken>,
}

/// One ordered part within an assistant turn. The variant order in `parts`
/// reflects the original stream order so persistence and request replay can
/// reconstruct the assistant message byte-identically.
#[derive(Debug, Clone)]
pub enum AssistantPart {
    Reasoning(ThinkingBuilder),
    Text(TextPartBuilder),
    ToolUse(ToolUseBuilder),
}

/// Finalized tool use with parsed input (ready for execution).
#[derive(Debug, Clone)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub id_origin: zdx_types::IdOrigin,
    pub replay: Option<ReplayToken>,
}

/// Builder for accumulating all assistant turn content from streaming events.
///
/// `parts` preserves the original stream order across text, reasoning, and
/// tool-use parts. The Gemini implicit prompt cache requires byte-identical
/// replay of assistant turns, so category-bucketed reordering (the previous
/// design) is no longer permitted — the persistence and request layers walk
/// `parts` in arrival order.
///
/// `model` is the source model id for this turn; it is threaded into any
/// per-part `ReplayToken::Gemini` so the next-turn request builder can gate
/// signature replay to the same model (Gemini's exact-model match).
#[derive(Debug, Clone, Default)]
pub struct AssistantTurnBuilder {
    pub model: String,
    pub parts: Vec<AssistantPart>,
}

impl AssistantTurnBuilder {
    pub fn new(model: String) -> Self {
        Self {
            model,
            parts: Vec::new(),
        }
    }

    /// Returns the concatenated text from all `Text` parts (in stream order).
    /// Used for the cumulative `final_text` carried on `AssistantCompleted`
    /// and `TurnFinished` events.
    pub fn final_text(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            if let AssistantPart::Text(tb) = part {
                out.push_str(&tb.text);
            }
        }
        out
    }

    /// Returns true when at least one tool-use part is present.
    pub fn has_tool_uses(&self) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, AssistantPart::ToolUse(_)))
    }

    /// Iterator over tool-use builders in stream order.
    pub fn tool_uses(&self) -> impl Iterator<Item = &ToolUseBuilder> {
        self.parts.iter().filter_map(|p| match p {
            AssistantPart::ToolUse(t) => Some(t),
            _ => None,
        })
    }

    /// Finds a tool use builder by stream index.
    pub fn find_tool_use_mut(&mut self, index: usize) -> Option<&mut ToolUseBuilder> {
        self.parts.iter_mut().find_map(|p| match p {
            AssistantPart::ToolUse(t) if t.index == index => Some(t),
            _ => None,
        })
    }

    /// Finds a thinking block by stream index.
    pub fn find_thinking_mut(&mut self, index: usize) -> Option<&mut ThinkingBuilder> {
        self.parts.iter_mut().find_map(|p| match p {
            AssistantPart::Reasoning(t) if t.index == index => Some(t),
            _ => None,
        })
    }

    /// Finds a text-part builder by stream index.
    pub fn find_text_mut(&mut self, index: usize) -> Option<&mut TextPartBuilder> {
        self.parts.iter_mut().find_map(|p| match p {
            AssistantPart::Text(t) if t.index == index => Some(t),
            _ => None,
        })
    }

    /// Returns a mutable reference to the text part for `index`, creating it
    /// if missing. This is the path used by `TextDelta` events from
    /// providers (Anthropic / `OpenAI`) that don't always emit an explicit
    /// `ContentBlockStart` for text blocks before deltas arrive.
    ///
    /// # Panics
    /// Panics only if the just-pushed text part cannot be found again on
    /// the trailing scan; this is impossible by construction.
    pub fn ensure_text_part_mut(&mut self, index: usize) -> &mut TextPartBuilder {
        let exists = self
            .parts
            .iter()
            .any(|p| matches!(p, AssistantPart::Text(t) if t.index == index));
        if !exists {
            self.parts.push(AssistantPart::Text(TextPartBuilder {
                index,
                text: String::new(),
                replay: None,
            }));
        }
        self.parts
            .iter_mut()
            .rev()
            .find_map(|p| match p {
                AssistantPart::Text(t) if t.index == index => Some(t),
                _ => None,
            })
            .expect("text part just pushed or already present")
    }

    /// Pushes a new `ToolUseBuilder`. Caller is responsible for setting
    /// `id_origin` to `Real` when the provider emitted an id; defaults to
    /// `Synthesized` for the engine-side path that fills in a placeholder.
    pub fn push_tool_use(&mut self, builder: ToolUseBuilder) {
        self.parts.push(AssistantPart::ToolUse(builder));
    }

    /// Pushes a new `ThinkingBuilder`.
    pub fn push_reasoning(&mut self, builder: ThinkingBuilder) {
        self.parts.push(AssistantPart::Reasoning(builder));
    }

    /// Walks `parts` in stream order and produces the finalized assistant
    /// content. Tool-use parts with malformed JSON inputs are emitted as
    /// `ChatContentBlock::ToolUse` with a sentinel `__zdx_invalid_json__`
    /// input so persistence and downstream diagnostics retain the raw
    /// payload; their ids are also collected separately as malformed.
    pub fn finalize(self) -> FinalizedAssistantTurn {
        let mut blocks: Vec<ChatContentBlock> = Vec::with_capacity(self.parts.len());
        let mut executable: Vec<ToolUse> = Vec::new();
        let mut all_tool_uses: Vec<ToolUse> = Vec::new();
        let mut malformed_results: Vec<ToolResult> = Vec::new();
        let mut malformed_tools: Vec<(String, String, ToolOutput)> = Vec::new();
        let mut diagnostics: Vec<TurnDiagnostic> = Vec::new();
        let mut final_text = String::new();

        for part in self.parts {
            match part {
                AssistantPart::Reasoning(tb) => {
                    if tb.text.is_empty() && tb.replay.is_none() {
                        // Drop empty reasoning placeholders that never received
                        // any deltas or replay metadata; they would otherwise
                        // serialize as `{ "type": "reasoning" }` with no body.
                        continue;
                    }
                    let text = (!tb.text.is_empty()).then_some(tb.text);
                    blocks.push(ChatContentBlock::Reasoning(ReasoningBlock {
                        text,
                        replay: tb.replay,
                    }));
                }
                AssistantPart::Text(tb) => {
                    if tb.text.is_empty() {
                        continue;
                    }
                    final_text.push_str(&tb.text);
                    blocks.push(ChatContentBlock::Text {
                        text: tb.text,
                        replay: tb.replay,
                    });
                }
                AssistantPart::ToolUse(tu) => {
                    let id_origin = tu.id_origin;
                    let replay = tu.replay.clone();
                    match serde_json::from_str::<Value>(&tu.input_json) {
                        Ok(input) => {
                            let tool_use = ToolUse {
                                id: tu.id.clone(),
                                name: tu.name.clone(),
                                input: input.clone(),
                                id_origin,
                                replay: replay.clone(),
                            };
                            executable.push(tool_use.clone());
                            all_tool_uses.push(tool_use);
                            blocks.push(ChatContentBlock::ToolUse {
                                id: tu.id,
                                name: tu.name,
                                input,
                                id_origin,
                                replay,
                            });
                        }
                        Err(err) => {
                            diagnostics.push(TurnDiagnostic::Parse {
                                message: format!(
                                    "Invalid tool input JSON for {}: {}",
                                    tu.name, err
                                ),
                                details: Some(truncate_for_error(&tu.input_json, 500)),
                            });
                            let sentinel = malformed_tool_input_value(&tu.input_json);
                            let error_output = invalid_tool_output(&tu.input_json, &err);
                            malformed_results
                                .push(ToolResult::from_output(tu.id.clone(), &error_output));
                            malformed_tools.push((tu.id.clone(), tu.name.clone(), error_output));
                            all_tool_uses.push(ToolUse {
                                id: tu.id.clone(),
                                name: tu.name.clone(),
                                input: sentinel.clone(),
                                id_origin,
                                replay: replay.clone(),
                            });
                            blocks.push(ChatContentBlock::ToolUse {
                                id: tu.id,
                                name: tu.name,
                                input: sentinel,
                                id_origin,
                                replay,
                            });
                        }
                    }
                }
            }
        }

        FinalizedAssistantTurn {
            blocks,
            executable,
            all_tool_uses,
            malformed_results,
            malformed_tools,
            diagnostics,
            final_text,
        }
    }
}

/// Result of finalizing an `AssistantTurnBuilder`. `blocks` is the ordered
/// assistant message content (`Reasoning`, `Text`, and `ToolUse` parts in
/// stream order, including malformed sentinels for unparseable tool inputs).
/// `executable` and `all_tool_uses` carry only the tool-use parts in the
/// same order; `executable` excludes malformed parts, while `all_tool_uses`
/// is used by interrupt-path message synthesis (which must produce a
/// matching `tool_results` block for every tool the assistant requested).
pub struct FinalizedAssistantTurn {
    pub blocks: Vec<ChatContentBlock>,
    pub executable: Vec<ToolUse>,
    pub all_tool_uses: Vec<ToolUse>,
    pub malformed_results: Vec<ToolResult>,
    pub malformed_tools: Vec<(String, String, ToolOutput)>,
    pub(crate) diagnostics: Vec<TurnDiagnostic>,
    pub final_text: String,
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
pub(crate) enum TurnDiagnostic {
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

fn emit_turn_error(err: &TurnError, sender: &EventSender, prior_message_count: usize) {
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
                prior_message_count,
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
                prior_message_count,
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
                prior_message_count,
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
                prior_message_count,
            });
        }
    }
}

fn emit_turn_error_with_messages(
    err: &TurnError,
    messages: &[ChatMessage],
    sender: &EventSender,
    prior_message_count: usize,
) {
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
                prior_message_count,
            });
        }
        other => {
            // For non-Provider errors, fall back to default behavior (no messages)
            emit_turn_error(other, sender, prior_message_count);
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
    let sender = EventSender::new(tx);
    let initial_message_count = messages.len();
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
                emit_turn_error(&err, &sender, initial_message_count);
            } else {
                emit_turn_error_with_messages(
                    &err,
                    &committed_messages,
                    &sender,
                    initial_message_count,
                );
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
    let _run_guard = crate::agent_activity::start(
        thread_id,
        options.surface.as_deref(),
        Some(setup.model.as_str()),
    );
    let mut messages = messages;
    let initial_message_count = messages.len();
    let mut consecutive_malformed_tool_turns = 0usize;

    loop {
        ensure_not_interrupted(None, cancel).map_err(|e| (e, messages.clone()))?;

        // Unified retry loop for transient provider errors.
        //
        // Covers two cases with the same backoff/telemetry:
        //   1. Pre-stream failures (connection errors, HTTP 5xx before SSE starts).
        //   2. Mid-stream SSE errors (e.g. Anthropic `overloaded_error` event)
        //      — but only when nothing user-visible has been emitted yet, so a
        //      retry doesn't force the UI to rewind partial text / tool calls /
        //      reasoning deltas.
        let mut stream_state = {
            let mut attempt: u32 = 0;
            'retry: loop {
                ensure_not_interrupted(None, cancel).map_err(|e| (e, messages.clone()))?;

                let outcome: std::result::Result<StreamState, (TurnError, bool)> =
                    match request_stream(
                        &setup.client,
                        &messages,
                        &setup.tools,
                        system_prompt,
                        cancel,
                    )
                    .await
                    {
                        Ok(stream) => {
                            match consume_stream(stream, &messages, sender, cancel, &setup.model)
                                .await
                            {
                                Ok(state) => Ok(state),
                                Err((err, state)) => {
                                    Err((err, can_transparently_retry_stream(&state)))
                                }
                            }
                        }
                        Err(err) => Err((err, true)),
                    };

                match outcome {
                    Ok(state) => break 'retry state,
                    Err((err, can_retry)) => {
                        let retry_err = match &err {
                            TurnError::Provider(p) if p.is_retryable() && can_retry => {
                                Some(p.clone())
                            }
                            _ => None,
                        };
                        let Some(retry_err) = retry_err else {
                            return Err((err, messages.clone()));
                        };
                        if attempt >= MAX_RETRIES {
                            return Err((err, messages.clone()));
                        }
                        attempt += 1;
                        let delay = RETRY_BASE_DELAY_MS * 2u64.pow(attempt - 1);
                        tracing::warn!(
                            attempt,
                            max = MAX_RETRIES,
                            delay_ms = delay,
                            error = %retry_err.message,
                            "Transient provider error, retrying"
                        );
                        // Surface the retry in the transcript so TUI/bot users
                        // can see that we're backing off instead of staring at
                        // a frozen spinner. Non-fatal: the turn continues.
                        sender.send(AgentEvent::ProviderRetry {
                            kind: ErrorKind::from(retry_err.kind.clone()),
                            message: retry_err.message.clone(),
                            details: retry_err.details.clone(),
                            attempt,
                            max_retries: MAX_RETRIES,
                            delay_ms: delay,
                        });
                        wait_for_retry_delay(Duration::from_millis(delay), cancel)
                            .await
                            .map_err(|e| (e, messages.clone()))?;
                    }
                }
            }
        };

        emit_stop_reason_notice(stream_state.stop_reason.as_deref(), sender);

        if stream_state.needs_tool_execution() {
            let stats = process_tool_turn(
                &mut messages,
                &mut stream_state.turn,
                &setup,
                sender,
                cancel,
                initial_message_count,
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
            initial_message_count,
        ));
    }
}

struct RunTurnSetup {
    model: String,
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
            service_tier: options.service_tier.as_deref().or({
                match provider {
                    ProviderKind::OpenAI if config.providers.openai.fast_mode => Some("priority"),
                    ProviderKind::OpenAICodex if config.providers.openai_codex.fast_mode => {
                        Some("priority")
                    }
                    _ => None,
                }
            }),
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
        model: selection.model,
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
    service_tier: Option<&'a str>,
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
    let thinking_effort = map_thinking_to_anthropic_effort(options.thinking_level, options.model);
    // Always emit a Gemini thinking config — even when ThinkingLevel::Off — so that
    // `Off` sends an explicit minimum-thinking config rather than omitting
    // `thinkingConfig` (which lets Gemini fall back to its default high reasoning).
    let gemini_thinking = Some(GeminiThinkingConfig::from_thinking_level(
        options.thinking_level,
        options.model,
    ));

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
                options.service_tier.map(std::string::ToString::to_string),
            ),
        ))),
        ProviderKind::OpenAI => build_openai_client(
            config,
            options.model,
            config.max_tokens,
            reasoning_effort,
            options.text_verbosity,
            cache_key,
            options.service_tier.map(std::string::ToString::to_string),
        ),
        ProviderKind::OpenRouter => {
            build_openrouter_client(config, options.model, reasoning_effort, cache_key)
        }
        ProviderKind::DeepSeek => build_deepseek_client(
            config,
            options.model,
            reasoning_effort,
            cache_key,
            thinking_enabled,
        ),
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
                GeminiThinkingConfig::from_thinking_level(options.thinking_level, options.model),
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
    service_tier: Option<String>,
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
            service_tier,
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

fn build_deepseek_client(
    config: &Config,
    model: &str,
    reasoning_effort: Option<String>,
    cache_key: Option<String>,
    thinking_enabled: bool,
) -> Result<ProviderClient> {
    Ok(ProviderClient::DeepSeek(DeepSeekClient::new(
        DeepSeekConfig::from_env(
            model.to_string(),
            config.max_tokens,
            config.providers.deepseek.effective_base_url(),
            config.providers.deepseek.effective_api_key(),
            cache_key,
            thinking_enabled,
            reasoning_effort,
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
    usage_seen: crate::providers::Usage,
    emitted_events: bool,
}

impl StreamState {
    fn new(model: String) -> Self {
        Self {
            turn: AssistantTurnBuilder::new(model),
            stop_reason: None,
            usage_seen: crate::providers::Usage::default(),
            emitted_events: false,
        }
    }

    fn needs_tool_execution(&self) -> bool {
        self.stop_reason.as_deref() == Some("tool_use") && self.turn.has_tool_uses()
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

async fn wait_for_retry_delay(
    delay: Duration,
    cancel: Option<&CancellationToken>,
) -> TurnResult<()> {
    tokio::select! {
        biased;
        () = interrupt::wait_for_interrupt() => Err(TurnError::interrupted(None)),
        () = wait_for_cancel(cancel) => Err(TurnError::interrupted(None)),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

/// Consumes a provider stream. On error, returns the accumulated `StreamState`
/// alongside the error so the caller can decide whether a transparent retry is
/// safe (i.e. nothing externally visible or persisted has been emitted yet).
async fn consume_stream(
    mut stream: ProviderStream,
    prior_messages: &[ChatMessage],
    sender: &EventSender,
    cancel: Option<&CancellationToken>,
    model: &str,
) -> std::result::Result<StreamState, (TurnError, StreamState)> {
    let mut state = StreamState::new(model.to_string());

    loop {
        if interrupt::is_interrupted() || cancel.is_some_and(CancellationToken::is_cancelled) {
            let turn = std::mem::take(&mut state.turn);
            return Err((interrupted_turn_from_stream(prior_messages, turn), state));
        }
        let event = match timeout(STREAM_POLL_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(event))) => event,
            Ok(Some(Err(err))) => return Err((TurnError::Provider(err), state)),
            Ok(None) => return Ok(state),
            Err(_) => continue,
        };
        if let Err(err) = handle_stream_event(event, sender, &mut state) {
            return Err((err, state));
        }
    }
}

fn can_transparently_retry_stream(state: &StreamState) -> bool {
    !state.emitted_events
}

#[allow(clippy::too_many_lines, clippy::match_same_arms)]
fn handle_stream_event(
    event: StreamEvent,
    sender: &EventSender,
    state: &mut StreamState,
) -> TurnResult<()> {
    match event {
        StreamEvent::TextDelta { index, text } if !text.is_empty() => {
            let part = state.turn.ensure_text_part_mut(index);
            part.text.push_str(&text);
            sender.send(AgentEvent::AssistantDelta { text });
            state.emitted_events = true;
        }
        StreamEvent::ContentBlockStart {
            index,
            block_type: ContentBlockType::ToolUse,
            id,
            name,
            id_origin,
            ..
        } => {
            handle_tool_content_start(index, id, name, id_origin, sender, &mut state.turn);
            state.emitted_events = true;
        }
        StreamEvent::ContentBlockStart {
            index,
            block_type: ContentBlockType::Text,
            ..
        } => {
            // Lazily create the text part so subsequent `TextDelta`s and the
            // matching `ContentBlockCompleted` can attach to it. Idempotent
            // for providers that emit deltas without an explicit start.
            let _ = state.turn.ensure_text_part_mut(index);
        }
        StreamEvent::ContentBlockStart {
            index,
            block_type: ContentBlockType::Reasoning,
            ..
        } => {
            start_reasoning_block(&mut state.turn, index, None);
        }
        StreamEvent::ContentBlockStart {
            index,
            block_type: ContentBlockType::RedactedThinking,
            data,
            ..
        } => {
            let Some(data) = data.filter(|s| !s.is_empty()) else {
                return Err(TurnError::Parse {
                    message: "Anthropic redacted_thinking content_block_start requires a non-empty `data` field"
                        .to_string(),
                    details: None,
                });
            };
            start_reasoning_block(
                &mut state.turn,
                index,
                Some(ReplayToken::AnthropicRedacted { data }),
            );
        }
        StreamEvent::InputJsonDelta {
            index,
            partial_json,
        } => {
            if handle_input_json_delta(index, &partial_json, sender, &mut state.turn) {
                state.emitted_events = true;
            }
        }
        StreamEvent::ReasoningDelta { index, reasoning } => {
            if let Some(tb) = state.turn.find_thinking_mut(index) {
                if !reasoning.is_empty() {
                    tb.had_delta = true;
                }
                tb.text.push_str(&reasoning);
                sender.send(AgentEvent::ReasoningDelta { text: reasoning });
                state.emitted_events = true;
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
        StreamEvent::ContentBlockCompleted { index, signature } => {
            if emit_reasoning_completion(sender, &mut state.turn, index) {
                state.emitted_events = true;
            }
            if emit_tool_input_completion(sender, &state.turn, index) {
                state.emitted_events = true;
            }
            // Per-part Gemini signatures (text + tool_use) ride this channel.
            // Reasoning signatures still flow through `ReasoningSignatureDelta`
            // and were attached during `emit_reasoning_completion` above, so
            // we ignore signatures on reasoning indices here.
            if let Some((sig_provider, sig)) = signature {
                attach_part_signature(&mut state.turn, index, sig_provider, sig);
            }
        }
        StreamEvent::MessageDelta { stop_reason, usage } => {
            state.stop_reason = stop_reason;
            if emit_message_delta_usage(sender, &mut state.usage_seen, usage) {
                state.emitted_events = true;
            }
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
        StreamEvent::MessageStart { usage, .. } => {
            if emit_message_start_usage(sender, &mut state.usage_seen, &usage) {
                state.emitted_events = true;
            }
        }
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
        StreamEvent::TextDelta { .. }
        | StreamEvent::MessageCompleted
        | StreamEvent::Ping
        | StreamEvent::Ignored { .. } => {}
    }
    Ok(())
}

fn handle_tool_content_start(
    index: usize,
    id: Option<String>,
    name: Option<String>,
    id_origin: Option<zdx_types::IdOrigin>,
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
    turn.push_tool_use(ToolUseBuilder {
        index,
        id: tool_id,
        name: tool_name,
        input_json: String::new(),
        input_preview_len: 0,
        id_origin: id_origin.unwrap_or_default(),
        replay: None,
    });
}

/// Attaches a per-part Gemini signature (delivered on `ContentBlockCompleted`
/// for text and tool-use parts) as a `ReplayToken::Gemini` carrying the
/// source model. Reasoning parts route their signature through
/// `ReasoningSignatureDelta` and are not handled here.
fn attach_part_signature(
    turn: &mut AssistantTurnBuilder,
    index: usize,
    provider: crate::providers::SignatureProvider,
    signature: String,
) {
    let model = turn.model.clone();
    let token = match provider {
        crate::providers::SignatureProvider::Gemini => ReplayToken::Gemini { signature, model },
        crate::providers::SignatureProvider::Anthropic => ReplayToken::Anthropic { signature },
    };
    if let Some(text_part) = turn.find_text_mut(index) {
        text_part.replay = Some(token);
        return;
    }
    if let Some(tool_part) = turn.find_tool_use_mut(index) {
        tool_part.replay = Some(token);
    }
}

/// Appends a new `ThinkingBuilder` to a turn for a reasoning-style
/// `content_block_start`. `replay` is pre-populated for
/// `redacted_thinking` (the `data` blob IS the block) and left `None`
/// for normal `thinking` blocks that accumulate text and signature
/// through subsequent deltas.
fn start_reasoning_block(
    turn: &mut AssistantTurnBuilder,
    index: usize,
    replay: Option<ReplayToken>,
) {
    turn.push_reasoning(ThinkingBuilder {
        index,
        text: String::new(),
        signature: String::new(),
        signature_provider: None,
        replay,
        had_delta: false,
    });
}

fn handle_input_json_delta(
    index: usize,
    partial_json: &str,
    sender: &EventSender,
    turn: &mut AssistantTurnBuilder,
) -> bool {
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
            return true;
        }
    }
    false
}

fn emit_message_delta_usage(
    sender: &EventSender,
    usage_seen: &mut crate::providers::Usage,
    usage: Option<crate::providers::UsageDelta>,
) -> bool {
    if let Some(u) = usage {
        let delta = u.incremental_from(usage_seen);
        u.apply_to(usage_seen);

        if !delta.is_empty() {
            sender.send(AgentEvent::UsageUpdate {
                input_tokens: delta.input_tokens,
                output_tokens: delta.output_tokens,
                cache_read_input_tokens: delta.cache_read_input_tokens,
                cache_creation_input_tokens: delta.cache_creation_input_tokens,
            });
            return true;
        }
    }
    false
}

fn emit_message_start_usage(
    sender: &EventSender,
    usage_seen: &mut crate::providers::Usage,
    usage: &crate::providers::Usage,
) -> bool {
    *usage_seen = usage.clone();
    sender.send(AgentEvent::UsageUpdate {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
    });
    true
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

fn emit_reasoning_completion(
    sender: &EventSender,
    turn: &mut AssistantTurnBuilder,
    index: usize,
) -> bool {
    let model = turn.model.clone();
    if let Some(tb) = turn.find_thinking_mut(index) {
        if tb.replay.is_none()
            && !tb.signature.is_empty()
            && let Some(signature_provider) = tb.signature_provider
        {
            tb.replay = Some(match signature_provider {
                crate::providers::SignatureProvider::Gemini => ReplayToken::Gemini {
                    signature: tb.signature.clone(),
                    model: model.clone(),
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
        return true;
    }
    false
}

fn emit_tool_input_completion(
    sender: &EventSender,
    turn: &AssistantTurnBuilder,
    index: usize,
) -> bool {
    if let Some(tu) = turn.tool_uses().find(|t| t.index == index) {
        // With fine-grained tool streaming (`eager_input_streaming: true`),
        // Anthropic may legitimately deliver partial/invalid JSON if a tool
        // call is truncated (e.g. `max_tokens` reached). Preserve the raw
        // payload under a sentinel field so persistence and downstream
        // diagnostics can still see what was emitted, instead of silently
        // collapsing to `{}` and discarding the evidence.
        let input: Value = serde_json::from_str(&tu.input_json)
            .unwrap_or_else(|_| malformed_tool_input_value(&tu.input_json));
        sender.send(AgentEvent::ToolInputCompleted {
            id: tu.id.clone(),
            name: tu.name.clone(),
            input,
        });
        return true;
    }
    false
}

fn malformed_tool_input_value(input_json: &str) -> Value {
    serde_json::json!({
        "__zdx_invalid_json__": true,
        "raw": input_json,
    })
}

fn invalid_tool_output(input_json: &str, error: &serde_json::Error) -> ToolOutput {
    ToolOutput::failure(
        "invalid_json",
        format!("Failed to parse tool arguments: {error}"),
        Some(truncate_for_error(input_json, 500)),
    )
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
    prior_message_count: usize,
) -> TurnResult<ToolTurnStats> {
    let finalized = std::mem::take(turn).finalize();
    let executable_count = finalized.executable.len();
    let malformed_count = finalized.malformed_results.len();
    emit_assistant_completed_if_present(sender, &finalized.final_text);
    emit_turn_diagnostics(&finalized.diagnostics, sender);
    emit_malformed_tool_events(sender, finalized.malformed_tools);

    let turn_text = finalized.final_text;
    messages.push(ChatMessage::assistant_blocks(finalized.blocks));

    let mut tool_results = execute_tools_async(
        &finalized.executable,
        &setup.tool_ctx,
        &setup.enabled_tools,
        sender,
        &setup.tool_registry,
        cancel,
    )
    .await;
    tool_results.extend(finalized.malformed_results);
    messages.push(ChatMessage::tool_results(tool_results));

    if interrupt::is_interrupted() || cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(TurnError::interrupted_with_completion(
            (!turn_text.is_empty()).then_some(turn_text.clone()),
            turn_text,
            messages.clone(),
        ));
    }

    // Emit a non-terminal checkpoint so persistence flushes the new turn
    // suffix (assistant blocks + tool_results) without waiting for the
    // terminal `TurnFinished`. Long tool loops persist incrementally between
    // tool turns. UI consumers can ignore this; they get live state from
    // streaming events.
    sender.send(AgentEvent::TurnCheckpoint {
        messages: messages.clone(),
        prior_message_count,
    });

    Ok(ToolTurnStats {
        executable: executable_count,
        malformed: malformed_count,
    })
}

fn finalize_non_tool_turn(
    messages: &mut Vec<ChatMessage>,
    turn: AssistantTurnBuilder,
    sender: &EventSender,
    prior_message_count: usize,
) -> (String, Vec<ChatMessage>) {
    let finalized = turn.finalize();
    let final_text = finalized.final_text;
    emit_assistant_completed_if_present(sender, &final_text);
    if !finalized.blocks.is_empty() {
        messages.push(ChatMessage::assistant_blocks(finalized.blocks));
    }
    sender.send(AgentEvent::TurnFinished {
        status: TurnStatus::Completed,
        final_text: final_text.clone(),
        messages: messages.clone(),
        prior_message_count,
    });
    (final_text, messages.clone())
}

fn interrupted_turn_from_stream(
    prior_messages: &[ChatMessage],
    turn: AssistantTurnBuilder,
) -> TurnError {
    let final_text = turn.final_text();
    let messages = build_interrupted_messages(prior_messages, turn);
    TurnError::interrupted_with_completion(
        (!final_text.is_empty()).then_some(final_text.clone()),
        final_text,
        messages,
    )
}

fn build_interrupted_messages(
    prior_messages: &[ChatMessage],
    turn: AssistantTurnBuilder,
) -> Vec<ChatMessage> {
    let finalized = turn.finalize();
    let mut messages = prior_messages.to_vec();

    if !finalized.blocks.is_empty() {
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            phase: Some("commentary".to_string()),
            content: crate::providers::MessageContent::Blocks(finalized.blocks),
        });
    }

    // For every tool the assistant requested (parseable or malformed), the
    // interrupted path must also surface a matching tool_result so the
    // assistant→user pair stays balanced for the next-turn replay. Parseable
    // tools get a `canceled` output; malformed tools keep their parse-error
    // output (already collected during `finalize`).
    if !finalized.all_tool_uses.is_empty() {
        let mut tool_results: Vec<ToolResult> = Vec::with_capacity(finalized.all_tool_uses.len());
        let mut malformed_results = finalized.malformed_results.into_iter();
        for tu in finalized.all_tool_uses {
            // Detect malformed by checking the sentinel input shape; if so,
            // pull the matching parse-error result. Otherwise synthesize a
            // canceled result.
            let is_malformed = tu
                .input
                .get("__zdx_invalid_json__")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if is_malformed {
                if let Some(result) = malformed_results.next() {
                    tool_results.push(result);
                }
            } else {
                let interrupted_output = ToolOutput::canceled("Interrupted by user");
                tool_results.push(ToolResult::from_output(tu.id, &interrupted_output));
            }
        }
        // Drain any extra malformed results that were not paired (defensive;
        // should not happen with the in-order walk above).
        for extra in malformed_results {
            tool_results.push(extra);
        }
        if !tool_results.is_empty() {
            messages.push(ChatMessage::tool_results(tool_results));
        }
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

fn map_thinking_to_anthropic_effort(
    level: ThinkingLevel,
    model: &str,
) -> Option<AnthropicEffortLevel> {
    if matches!(level, ThinkingLevel::Off) {
        return None;
    }

    // Strip provider-prefixed model ids like "claude-cli:claude-opus-4-7".
    let normalized = model.rsplit(':').next().unwrap_or(model);

    // Opus 4.7 introduced an `xhigh` effort level between `high` and `max`,
    // giving it 5 effort levels (low/medium/high/xhigh/max) — perfect 1:1
    // alignment with our 5 active ThinkingLevels.
    if normalized.starts_with("claude-opus-4-7") {
        return Some(match level {
            ThinkingLevel::Off => unreachable!(),
            ThinkingLevel::Minimal => AnthropicEffortLevel::Low,
            ThinkingLevel::Low => AnthropicEffortLevel::Medium,
            ThinkingLevel::Medium => AnthropicEffortLevel::High,
            ThinkingLevel::High => AnthropicEffortLevel::XHigh,
            ThinkingLevel::XHigh => AnthropicEffortLevel::Max,
        });
    }

    // Opus/Sonnet 4.6 and earlier expose at most 4 effort levels
    // (low/medium/high/max). Collapse Minimal+Low into `low`.
    Some(match level {
        ThinkingLevel::Off => unreachable!(),
        ThinkingLevel::Minimal | ThinkingLevel::Low => AnthropicEffortLevel::Low,
        ThinkingLevel::Medium => AnthropicEffortLevel::Medium,
        ThinkingLevel::High => AnthropicEffortLevel::High,
        ThinkingLevel::XHigh => AnthropicEffortLevel::Max,
    })
}

/// Surface non-`tool_use`/`end_turn` stop reasons that warrant explicit
/// user feedback (introduced in Claude 4.5+ and reaffirmed in the 4.6/4.7
/// migration guide). The turn still completes — this is informational so
/// the UI can show what happened and the thread log can persist it.
fn emit_stop_reason_notice(stop_reason: Option<&str>, sender: &EventSender) {
    let (kind, message, details) = match stop_reason {
        Some("refusal") => (
            NoticeKind::Refusal,
            "Claude declined to respond to this request.",
            "stop_reason=refusal",
        ),
        Some("model_context_window_exceeded") => (
            NoticeKind::ContextWindowExceeded,
            "Generation stopped: model context window exceeded. Start a new thread or trim history.",
            "stop_reason=model_context_window_exceeded",
        ),
        _ => return,
    };
    sender.send(AgentEvent::Notice {
        kind,
        message: message.to_string(),
        details: Some(details.to_string()),
    });
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
    fn anthropic_effort_opus_47_uses_one_to_one_shift_with_xhigh() {
        let m = "claude-opus-4-7";
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::Off, m),
            None
        );
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::Minimal, m),
            Some(AnthropicEffortLevel::Low)
        );
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::Low, m),
            Some(AnthropicEffortLevel::Medium)
        );
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::Medium, m),
            Some(AnthropicEffortLevel::High)
        );
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::High, m),
            Some(AnthropicEffortLevel::XHigh)
        );
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::XHigh, m),
            Some(AnthropicEffortLevel::Max)
        );
    }

    #[test]
    fn anthropic_effort_opus_46_collapses_minimal_low_and_skips_xhigh() {
        let m = "claude-opus-4-6";
        // 4.6 has no `xhigh`; High must stay at `high` (not be promoted).
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::Minimal, m),
            Some(AnthropicEffortLevel::Low)
        );
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::Low, m),
            Some(AnthropicEffortLevel::Low)
        );
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::Medium, m),
            Some(AnthropicEffortLevel::Medium)
        );
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::High, m),
            Some(AnthropicEffortLevel::High)
        );
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::XHigh, m),
            Some(AnthropicEffortLevel::Max)
        );
    }

    #[test]
    fn anthropic_effort_normalizes_provider_prefixed_model_id() {
        // claude-cli:claude-opus-4-7 should still hit the 4.7 branch.
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::High, "claude-cli:claude-opus-4-7"),
            Some(AnthropicEffortLevel::XHigh)
        );
        // And 4.6 should still collapse High → high under the cli prefix.
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::High, "claude-cli:claude-opus-4-6"),
            Some(AnthropicEffortLevel::High)
        );
    }

    #[test]
    fn stop_reason_notice_emits_for_refusal() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = EventSender::new(tx);
        emit_stop_reason_notice(Some("refusal"), &sender);
        let evt = rx.try_recv().expect("expected an event");
        match &*evt {
            AgentEvent::Notice { kind, message, .. } => {
                assert_eq!(*kind, NoticeKind::Refusal);
                assert!(message.contains("declined"), "got: {message}");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn stop_reason_notice_emits_for_context_window_exceeded() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = EventSender::new(tx);
        emit_stop_reason_notice(Some("model_context_window_exceeded"), &sender);
        let evt = rx.try_recv().expect("expected an event");
        match &*evt {
            AgentEvent::Notice { kind, message, .. } => {
                assert_eq!(*kind, NoticeKind::ContextWindowExceeded);
                assert!(
                    message.contains("context window exceeded"),
                    "got: {message}"
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn stop_reason_notice_is_silent_for_normal_reasons() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = EventSender::new(tx);
        emit_stop_reason_notice(Some("end_turn"), &sender);
        emit_stop_reason_notice(Some("tool_use"), &sender);
        emit_stop_reason_notice(None, &sender);
        assert!(rx.try_recv().is_err(), "no event should be emitted");
    }

    #[test]
    fn emit_tool_input_completion_preserves_malformed_json_under_sentinel() {
        // With eager_input_streaming GA, Anthropic may legitimately emit
        // truncated/invalid JSON when max_tokens is reached mid-tool.
        // We must not silently collapse the payload to `{}` and lose the
        // raw evidence — persistence and diagnostics need to see it.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = EventSender::new(tx);

        let mut turn = AssistantTurnBuilder::new(String::new());
        turn.push_tool_use(ToolUseBuilder {
            index: 0,
            id: "toolu_xyz".to_string(),
            name: "write".to_string(),
            input_json: "{\"path\":\"a.txt\",\"content\":\"hel".to_string(), // truncated
            input_preview_len: 0,
            id_origin: zdx_types::IdOrigin::Synthesized,
            replay: None,
        });

        emit_tool_input_completion(&sender, &turn, 0);
        let evt = rx.try_recv().expect("expected event");
        match &*evt {
            AgentEvent::ToolInputCompleted { id, name, input } => {
                assert_eq!(id, "toolu_xyz");
                assert_eq!(name, "write");
                assert_eq!(input["__zdx_invalid_json__"], serde_json::json!(true));
                assert_eq!(
                    input["raw"],
                    serde_json::json!("{\"path\":\"a.txt\",\"content\":\"hel")
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn emit_tool_input_completion_passes_through_valid_json() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = EventSender::new(tx);

        let mut turn = AssistantTurnBuilder::new(String::new());
        turn.push_tool_use(ToolUseBuilder {
            index: 0,
            id: "toolu_ok".to_string(),
            name: "read".to_string(),
            input_json: "{\"path\":\"a.txt\"}".to_string(),
            input_preview_len: 0,
            id_origin: zdx_types::IdOrigin::Synthesized,
            replay: None,
        });

        emit_tool_input_completion(&sender, &turn, 0);
        let evt = rx.try_recv().expect("expected event");
        match &*evt {
            AgentEvent::ToolInputCompleted { input, .. } => {
                assert_eq!(input["path"], serde_json::json!("a.txt"));
                assert!(input.get("__zdx_invalid_json__").is_none());
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn finalize_reuses_malformed_tool_input_sentinel() {
        let mut turn = AssistantTurnBuilder::new(String::new());
        turn.push_tool_use(ToolUseBuilder {
            index: 0,
            id: "tool_bad".to_string(),
            name: "write".to_string(),
            input_json: "{\"path\":\"a.txt\",\"content\":\"hel".to_string(),
            input_preview_len: 0,
            id_origin: zdx_types::IdOrigin::Synthesized,
            replay: None,
        });

        let finalized = turn.finalize();
        assert_eq!(finalized.executable.len(), 0);
        assert_eq!(finalized.all_tool_uses.len(), 1);
        assert_eq!(
            finalized.all_tool_uses[0].input,
            malformed_tool_input_value("{\"path\":\"a.txt\",\"content\":\"hel")
        );
    }

    #[test]
    fn message_delta_usage_is_emitted_as_delta_from_cumulative_counts() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = EventSender::new(tx);
        let mut state = StreamState::new(String::new());

        handle_stream_event(
            StreamEvent::MessageStart {
                model: "claude-opus-4-7".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 100,
                    output_tokens: 2,
                    cache_read_input_tokens: 5,
                    cache_creation_input_tokens: 1,
                },
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::MessageDelta {
                stop_reason: None,
                usage: Some(crate::providers::UsageDelta {
                    input_tokens: None,
                    output_tokens: Some(10),
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                }),
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::MessageDelta {
                stop_reason: Some("end_turn".to_string()),
                usage: Some(crate::providers::UsageDelta {
                    input_tokens: Some(120),
                    output_tokens: Some(15),
                    cache_read_input_tokens: Some(8),
                    cache_creation_input_tokens: Some(1),
                }),
            },
            &sender,
            &mut state,
        )
        .unwrap();

        let usage_updates: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|event| match &*event {
                AgentEvent::UsageUpdate {
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                } => Some((
                    *input_tokens,
                    *output_tokens,
                    *cache_read_input_tokens,
                    *cache_creation_input_tokens,
                )),
                _ => None,
            })
            .collect();

        assert_eq!(
            usage_updates,
            vec![(100, 2, 5, 1), (0, 8, 0, 0), (20, 5, 3, 0)]
        );
    }

    /// Feeds a `redacted_thinking` `content_block_start` (with opaque
    /// `data`) followed by a `content_block_stop` through
    /// `handle_stream_event` and asserts the turn builder captured the
    /// block as an empty-text `ReasoningBlock` carrying a
    /// `ReplayToken::AnthropicRedacted` for next-turn replay.
    #[test]
    fn handle_stream_event_captures_redacted_thinking_block() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = EventSender::new(tx);
        let mut state = StreamState::new(String::new());

        handle_stream_event(
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::RedactedThinking,
                id: None,
                name: None,
                data: Some("enc".to_string()),
                id_origin: None,
            },
            &sender,
            &mut state,
        )
        .expect("redacted_thinking start should be accepted");

        handle_stream_event(
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None,
            },
            &sender,
            &mut state,
        )
        .expect("content_block_stop should be accepted");

        let reasoning_parts: Vec<&ThinkingBuilder> = state
            .turn
            .parts
            .iter()
            .filter_map(|p| match p {
                AssistantPart::Reasoning(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(reasoning_parts.len(), 1);
        let tb = reasoning_parts[0];
        assert_eq!(tb.index, 0);
        assert!(tb.text.is_empty());
        assert_eq!(
            tb.replay,
            Some(ReplayToken::AnthropicRedacted {
                data: "enc".to_string()
            })
        );

        // `content_block_stop` should have emitted a `ReasoningCompleted`
        // event carrying the replay token but no plain-text summary.
        let evt = rx.try_recv().expect("expected ReasoningCompleted event");
        match &*evt {
            AgentEvent::ReasoningCompleted { block } => {
                assert!(block.text.is_none());
                assert_eq!(
                    block.replay,
                    Some(ReplayToken::AnthropicRedacted {
                        data: "enc".to_string()
                    })
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // Finalizing should emit a `Reasoning` block with empty text and
        // the redacted replay token — exactly what the outbound request
        // path needs to serialize as `redacted_thinking`.
        let finalized = state.turn.finalize();
        let blocks = finalized.blocks;
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            ChatContentBlock::Reasoning(ReasoningBlock {
                text: None,
                replay: Some(ReplayToken::AnthropicRedacted { data })
            }) if data == "enc"
        ));
    }

    /// A `redacted_thinking` `content_block_start` that is missing the
    /// required `data` field is a protocol violation — the engine must
    /// surface a `TurnError::Parse` rather than silently capturing an
    /// empty block that would be unreplayable on the next turn.
    #[test]
    fn handle_stream_event_rejects_redacted_thinking_without_data() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = EventSender::new(tx);
        let mut state = StreamState::new(String::new());

        let err = handle_stream_event(
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::RedactedThinking,
                id: None,
                name: None,
                data: None,
                id_origin: None,
            },
            &sender,
            &mut state,
        )
        .expect_err("missing data must be a parse error");

        match err {
            TurnError::Parse { message, details } => {
                assert!(
                    message.contains("redacted_thinking"),
                    "unexpected message: {message}"
                );
                assert!(details.is_none());
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(
            !state
                .turn
                .parts
                .iter()
                .any(|p| matches!(p, AssistantPart::Reasoning(_))),
            "no thinking block should be recorded on parse error"
        );
    }

    /// A `redacted_thinking` `content_block_start` that carries an
    /// explicit empty-string `data` field is also a protocol violation.
    /// The engine guard must mirror the SSE-parser boundary and reject
    /// it with the same diagnostic so a malformed event injected via a
    /// non-SSE producer (synthetic events, corrupted-JSONL rehydration)
    /// cannot silently persist an unreplayable empty replay token.
    #[test]
    fn handle_stream_event_rejects_redacted_thinking_with_empty_data() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = EventSender::new(tx);
        let mut state = StreamState::new(String::new());

        let err = handle_stream_event(
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::RedactedThinking,
                id: None,
                name: None,
                data: Some(String::new()),
                id_origin: None,
            },
            &sender,
            &mut state,
        )
        .expect_err("empty data must be a parse error");

        match err {
            TurnError::Parse { message, details } => {
                assert!(
                    message.contains("redacted_thinking"),
                    "unexpected message: {message}"
                );
                assert!(message.contains("`data`"), "unexpected message: {message}");
                assert!(details.is_none());
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(
            !state
                .turn
                .parts
                .iter()
                .any(|p| matches!(p, AssistantPart::Reasoning(_))),
            "no thinking block should be recorded on parse error"
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
            id_origin: zdx_types::IdOrigin::Synthesized,
            replay: None,
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
                id_origin: zdx_types::IdOrigin::Synthesized,
                replay: None,
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
                id_origin: zdx_types::IdOrigin::Synthesized,
                replay: None,
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
        emit_turn_error(&err, &sender, 0);

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
                ..
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
        emit_turn_error(&err, &sender, 0);

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
                ..
            } if final_text == "partial" && event_messages == &messages
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn interrupted_stream_snapshot_preserves_partial_context_for_next_turn() {
        use crate::providers::{ChatContentBlock, MessageContent};
        use crate::tools::ToolResultContent;

        let prior_messages = vec![ChatMessage::user("analyze the repo")];
        let mut turn = AssistantTurnBuilder::new(String::new());
        turn.push_reasoning(ThinkingBuilder {
            index: 0,
            text: "Let me inspect the project first.".to_string(),
            signature: String::new(),
            signature_provider: None,
            replay: Some(ReplayToken::Anthropic {
                signature: "sig123".to_string(),
            }),
            had_delta: true,
        });
        // Append a Text part that mimics what `TextDelta` would produce
        // for the assistant's accumulated text.
        turn.ensure_text_part_mut(2).text = "I found the provider loop.".to_string();
        turn.push_tool_use(ToolUseBuilder {
            index: 1,
            id: "tool_1".to_string(),
            name: "read".to_string(),
            input_json: r#"{"file_path":"src/main.rs"}"#.to_string(),
            input_preview_len: 0,
            id_origin: zdx_types::IdOrigin::Synthesized,
            replay: None,
        });

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
                    ChatContentBlock::Text { text, .. } if text == "I found the provider loop."
                ))
                && blocks.iter().any(|block| matches!(
                    block,
                    ChatContentBlock::ToolUse { id, name, input,
                            id_origin: zdx_types::IdOrigin::Synthesized,
                            replay: None }
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

    /// Verifies that finalizing a turn with malformed tool-use JSON
    /// produces a malformed sentinel rather than an executable tool.
    #[tokio::test]
    async fn test_finalize_records_malformed_tool_use_input() {
        let mut turn = AssistantTurnBuilder::new(String::new());
        turn.push_tool_use(ToolUseBuilder {
            index: 0,
            id: "tool1".to_string(),
            name: "test".to_string(),
            input_json: "{invalid json}".to_string(),
            input_preview_len: 0,
            id_origin: zdx_types::IdOrigin::Synthesized,
            replay: None,
        });

        let finalized = turn.finalize();
        assert!(finalized.executable.is_empty());
        assert_eq!(finalized.malformed_results.len(), 1);
        assert_eq!(finalized.all_tool_uses.len(), 1);
        assert_eq!(finalized.blocks.len(), 1);
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

    /// Transparent retries are only safe before the stream emits any external
    /// event to the UI/persistence pipeline.
    #[test]
    fn test_can_transparently_retry_stream_requires_no_emitted_events() {
        let mut state = StreamState::new(String::new());
        assert!(can_transparently_retry_stream(&state));

        state.emitted_events = true;
        assert!(!can_transparently_retry_stream(&state));
    }

    /// Verifies mid-stream retryable errors (e.g. Anthropic `overloaded_error`
    /// before any stream events are emitted remain transparently retryable.
    #[tokio::test]
    async fn test_consume_stream_returns_state_on_midstream_error() {
        use futures_util::stream;

        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![Err(
            ProviderError::api_error("overloaded_error", "API is temporarily overloaded"),
        )];
        let provider_stream: ProviderStream = Box::pin(stream::iter(events));

        let result = consume_stream(provider_stream, &[], &sender, None, "").await;
        let Err((err, state)) = result else {
            panic!("stream should error out");
        };
        assert!(matches!(
            &err,
            TurnError::Provider(p) if p.is_retryable() && p.message.contains("overloaded_error")
        ));
        assert!(
            can_transparently_retry_stream(&state),
            "no stream events were emitted, so retry should be safe"
        );
    }

    #[tokio::test]
    async fn test_consume_stream_marks_usage_update_as_retry_unsafe() {
        use futures_util::stream;

        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage::default(),
            }),
            Err(ProviderError::api_error(
                "overloaded_error",
                "API is temporarily overloaded",
            )),
        ];
        let provider_stream: ProviderStream = Box::pin(stream::iter(events));

        let result = consume_stream(provider_stream, &[], &sender, None, "").await;
        let Err((_err, state)) = result else {
            panic!("stream should error out");
        };

        assert!(
            !can_transparently_retry_stream(&state),
            "usage updates are externally emitted and make retry unsafe"
        );
    }

    #[tokio::test]
    async fn test_consume_stream_marks_reasoning_completion_as_retry_unsafe() {
        use futures_util::stream;

        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::RedactedThinking,
                id: None,
                name: None,
                data: Some("enc".to_string()),
                id_origin: None,
            }),
            Ok(StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None,
            }),
            Err(ProviderError::api_error(
                "overloaded_error",
                "API is temporarily overloaded",
            )),
        ];
        let provider_stream: ProviderStream = Box::pin(stream::iter(events));

        let result = consume_stream(provider_stream, &[], &sender, None, "").await;
        let Err((_err, state)) = result else {
            panic!("stream should error out");
        };

        assert!(
            !can_transparently_retry_stream(&state),
            "reasoning completion is persisted and makes retry unsafe"
        );
    }

    // ---------------------------------------------------------------------
    // Engine builder + model threading + checkpoint contracts.
    // ---------------------------------------------------------------------

    /// Feeds an interleaved stream of `[reasoning, text, tool_use, text,
    /// tool_use]` block starts/completions through the engine and asserts
    /// the resulting assistant blocks come out in the **same** stream order
    /// — never bucketed by category.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn test_assistant_turn_preserves_part_order() {
        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let mut state = StreamState::new("gemini-3-pro-preview".to_string());

        // index 0: reasoning
        handle_stream_event(
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::Reasoning,
                id: None,
                name: None,
                data: None,
                id_origin: None,
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::ReasoningDelta {
                index: 0,
                reasoning: "thinking".to_string(),
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None,
            },
            &sender,
            &mut state,
        )
        .unwrap();

        // index 1: text
        handle_stream_event(
            StreamEvent::ContentBlockStart {
                index: 1,
                block_type: ContentBlockType::Text,
                id: None,
                name: None,
                data: None,
                id_origin: None,
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::TextDelta {
                index: 1,
                text: "before-tool".to_string(),
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::ContentBlockCompleted {
                index: 1,
                signature: None,
            },
            &sender,
            &mut state,
        )
        .unwrap();

        // index 2: tool_use (real id)
        handle_stream_event(
            StreamEvent::ContentBlockStart {
                index: 2,
                block_type: ContentBlockType::ToolUse,
                id: Some("call_real_001".to_string()),
                name: Some("read_file".to_string()),
                data: None,
                id_origin: Some(zdx_types::IdOrigin::Real),
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::InputJsonDelta {
                index: 2,
                partial_json: "{\"path\":\"a\"}".to_string(),
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::ContentBlockCompleted {
                index: 2,
                signature: None,
            },
            &sender,
            &mut state,
        )
        .unwrap();

        // index 3: text
        handle_stream_event(
            StreamEvent::ContentBlockStart {
                index: 3,
                block_type: ContentBlockType::Text,
                id: None,
                name: None,
                data: None,
                id_origin: None,
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::TextDelta {
                index: 3,
                text: "between".to_string(),
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::ContentBlockCompleted {
                index: 3,
                signature: None,
            },
            &sender,
            &mut state,
        )
        .unwrap();

        // index 4: tool_use (synthesized id)
        handle_stream_event(
            StreamEvent::ContentBlockStart {
                index: 4,
                block_type: ContentBlockType::ToolUse,
                id: Some("synth_a".to_string()),
                name: Some("list_dir".to_string()),
                data: None,
                id_origin: Some(zdx_types::IdOrigin::Synthesized),
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::InputJsonDelta {
                index: 4,
                partial_json: "{}".to_string(),
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::ContentBlockCompleted {
                index: 4,
                signature: None,
            },
            &sender,
            &mut state,
        )
        .unwrap();

        let blocks = state.turn.finalize().blocks;
        assert_eq!(blocks.len(), 5, "got blocks: {blocks:#?}");
        let kinds: Vec<&'static str> = blocks
            .iter()
            .map(|b| match b {
                ChatContentBlock::Reasoning(_) => "reasoning",
                ChatContentBlock::Text { .. } => "text",
                ChatContentBlock::ToolUse { .. } => "tool_use",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["reasoning", "text", "tool_use", "text", "tool_use"]
        );

        // Verify id_origin was preserved per part.
        match &blocks[2] {
            ChatContentBlock::ToolUse { id, id_origin, .. } => {
                assert_eq!(id, "call_real_001");
                assert_eq!(*id_origin, zdx_types::IdOrigin::Real);
            }
            _ => panic!("expected tool_use at index 2"),
        }
        match &blocks[4] {
            ChatContentBlock::ToolUse { id_origin, .. } => {
                assert_eq!(*id_origin, zdx_types::IdOrigin::Synthesized);
            }
            _ => panic!("expected tool_use at index 4"),
        }
    }

    /// `ContentBlockCompleted.signature` carrying a Gemini signature on a
    /// text part must populate the resulting block's `replay` with a
    /// `ReplayToken::Gemini { signature, model }` where `model` matches the
    /// turn's source model.
    #[test]
    fn test_assistant_turn_gemini_signature_includes_model_on_text() {
        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let mut state = StreamState::new("gemini-3-pro-preview".to_string());

        handle_stream_event(
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::Text,
                id: None,
                name: None,
                data: None,
                id_origin: None,
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::TextDelta {
                index: 0,
                text: "hi".to_string(),
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: Some((
                    crate::providers::SignatureProvider::Gemini,
                    "sig_text".to_string(),
                )),
            },
            &sender,
            &mut state,
        )
        .unwrap();

        let blocks = state.turn.finalize().blocks;
        match &blocks[0] {
            ChatContentBlock::Text { text, replay } => {
                assert_eq!(text, "hi");
                assert_eq!(
                    replay,
                    &Some(ReplayToken::Gemini {
                        signature: "sig_text".to_string(),
                        model: "gemini-3-pro-preview".to_string(),
                    })
                );
            }
            other => panic!("expected text block, got {other:?}"),
        }
    }

    /// Same per-part signature contract for `tool_use` parts. The
    /// `ChatContentBlock::ToolUse.replay` field must carry the Gemini
    /// signature with the source model.
    #[test]
    fn test_assistant_turn_gemini_signature_includes_model_on_tool_use() {
        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let mut state = StreamState::new("gemini-3-pro-preview".to_string());

        handle_stream_event(
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::ToolUse,
                id: Some("call_real_001".to_string()),
                name: Some("read".to_string()),
                data: None,
                id_origin: Some(zdx_types::IdOrigin::Real),
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::InputJsonDelta {
                index: 0,
                partial_json: "{}".to_string(),
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: Some((
                    crate::providers::SignatureProvider::Gemini,
                    "sig_tool".to_string(),
                )),
            },
            &sender,
            &mut state,
        )
        .unwrap();

        let blocks = state.turn.finalize().blocks;
        match &blocks[0] {
            ChatContentBlock::ToolUse { replay, .. } => {
                assert_eq!(
                    replay,
                    &Some(ReplayToken::Gemini {
                        signature: "sig_tool".to_string(),
                        model: "gemini-3-pro-preview".to_string(),
                    })
                );
            }
            other => panic!("expected tool_use block, got {other:?}"),
        }
    }

    /// Reasoning blocks pick up their Gemini signature via the dedicated
    /// `ReasoningSignatureDelta` channel — the resulting `ReplayToken::Gemini`
    /// must also carry the source model.
    #[test]
    fn test_reasoning_completion_gemini_signature_includes_model() {
        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let mut state = StreamState::new("gemini-3-pro-preview".to_string());

        handle_stream_event(
            StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::Reasoning,
                id: None,
                name: None,
                data: None,
                id_origin: None,
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::ReasoningDelta {
                index: 0,
                reasoning: "thinking".to_string(),
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::ReasoningSignatureDelta {
                index: 0,
                signature: "sig_reason".to_string(),
                provider: crate::providers::SignatureProvider::Gemini,
            },
            &sender,
            &mut state,
        )
        .unwrap();
        handle_stream_event(
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None,
            },
            &sender,
            &mut state,
        )
        .unwrap();

        let blocks = state.turn.finalize().blocks;
        match &blocks[0] {
            ChatContentBlock::Reasoning(ReasoningBlock { replay, .. }) => {
                assert_eq!(
                    replay,
                    &Some(ReplayToken::Gemini {
                        signature: "sig_reason".to_string(),
                        model: "gemini-3-pro-preview".to_string(),
                    })
                );
            }
            other => panic!("expected reasoning block, got {other:?}"),
        }
    }

    /// `TurnCheckpoint` is emitted after every completed tool turn and
    /// carries the run-entry `prior_message_count` (the cursor stays
    /// stable across all checkpoints emitted within a single run).
    #[tokio::test]
    async fn test_turn_checkpoint_emitted_after_tool_turn() {
        use std::collections::HashSet;

        use crate::tools::{ToolContext, ToolRegistry};

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let mut turn = AssistantTurnBuilder::new("gemini-3-pro-preview".to_string());
        turn.push_tool_use(ToolUseBuilder {
            index: 0,
            id: "tool_no_op".to_string(),
            name: "nonexistent".to_string(),
            input_json: "{}".to_string(),
            input_preview_len: 0,
            id_origin: zdx_types::IdOrigin::Real,
            replay: None,
        });

        let setup = RunTurnSetup {
            model: "gemini-3-pro-preview".to_string(),
            client: ProviderClient::Gemini(GeminiClient::new(GeminiConfig {
                api_key: "x".to_string(),
                base_url: "https://example.invalid".to_string(),
                model: "gemini-3-pro-preview".to_string(),
                max_output_tokens: None,
                thinking_config: None,
            })),
            tools: Vec::new(),
            enabled_tools: HashSet::new(),
            tool_ctx: ToolContext::new(std::path::PathBuf::from("."), None),
            tool_registry: ToolRegistry::builtins(),
        };

        let mut messages: Vec<ChatMessage> = vec![ChatMessage::user("first turn")];
        let prior_count = messages.len();

        // Run the tool turn; the unknown tool name produces a tool_result
        // (failure) but the path still emits a TurnCheckpoint when it
        // completes successfully (no interrupt).
        process_tool_turn(&mut messages, &mut turn, &setup, &sender, None, prior_count)
            .await
            .expect("tool turn should complete");

        // Drain events looking for a TurnCheckpoint with our prior_count.
        let mut saw_checkpoint = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::TurnCheckpoint {
                messages: cp_messages,
                prior_message_count,
            } = &*event
            {
                assert_eq!(*prior_message_count, prior_count);
                assert!(cp_messages.len() > prior_count);
                saw_checkpoint = true;
            }
        }
        assert!(
            saw_checkpoint,
            "process_tool_turn must emit a TurnCheckpoint after a successful tool turn"
        );
    }

    /// Provider errors after a partial run carry the run-entry
    /// `prior_message_count` on the terminal `TurnFinished`. The cursor is
    /// captured at run entry, not at error time, so persistence can slice
    /// the new turn-suffix correctly.
    #[tokio::test]
    async fn test_turn_finished_cursor_in_provider_error_path() {
        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let messages = vec![
            ChatMessage::user("hi"),
            ChatMessage::assistant_text("partial reply", None),
        ];
        let err = TurnError::Provider(ProviderError::api_error("overloaded_error", "HTTP 502"));
        emit_turn_error_with_messages(&err, &messages, &sender, /* prior_message_count */ 1);

        let event = rx.try_recv().expect("expected event");
        match &*event {
            AgentEvent::TurnFinished {
                prior_message_count,
                messages: tf_messages,
                ..
            } => {
                assert_eq!(*prior_message_count, 1);
                assert_eq!(tf_messages.len(), 2);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    /// Even the no-message error path threads `prior_message_count` —
    /// callers that didn't accumulate any messages still pass the run-entry
    /// cursor. Persistence consumers tolerate `prior_count <= last_persisted`.
    #[tokio::test]
    async fn test_turn_finished_cursor_in_setup_failure() {
        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let err = TurnError::Internal(anyhow!("setup failed"));
        emit_turn_error(&err, &sender, /* prior_message_count */ 3);

        let event = rx.try_recv().expect("expected event");
        match &*event {
            AgentEvent::TurnFinished {
                prior_message_count,
                ..
            } => {
                assert_eq!(*prior_message_count, 3);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    /// `AssistantTurnBuilder::final_text` returns the concatenation of all
    /// text parts in stream order — used by the cumulative `final_text`
    /// carried on `AssistantCompleted` and `TurnFinished`.
    #[test]
    fn test_assistant_turn_final_text_concatenates_text_parts_in_order() {
        let mut turn = AssistantTurnBuilder::new("gemini-3-pro-preview".to_string());
        turn.ensure_text_part_mut(0).text = "first ".to_string();
        turn.push_reasoning(ThinkingBuilder {
            index: 1,
            text: "thinking".to_string(),
            signature: String::new(),
            signature_provider: None,
            replay: None,
            had_delta: true,
        });
        turn.ensure_text_part_mut(2).text = "second".to_string();

        assert_eq!(turn.final_text(), "first second");
    }

    /// Sanity: `AssistantTurnBuilder::new` stores the model so per-part
    /// Gemini replay tokens can pick it up via `attach_part_signature`.
    #[test]
    fn test_assistant_turn_builder_stores_model() {
        let turn = AssistantTurnBuilder::new("gemini-3-pro-preview".to_string());
        assert_eq!(turn.model, "gemini-3-pro-preview");
    }
}
