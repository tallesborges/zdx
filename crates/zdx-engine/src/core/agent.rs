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
use crate::providers::{
    ChatContentBlock, ChatMessage, ContentBlockType, ProviderBuildContext, ProviderError,
    ProviderKind, ProviderStream, ReasoningBlock, ReplayToken, StreamEvent, StreamingProvider,
    resolve_provider,
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
    /// Whether to register this run in the active-agents registry.
    /// Set `false` for utility/helper runs (title generation, TLDR, handoff,
    /// prompt builder, `read_thread`) so they do not count as "active agents".
    pub track_activity: bool,
    /// Logical role for this run, e.g. `"chat"`, `"exec"`, `"telegram"`,
    /// `"subagent"`. Surfaced in the active-agents registry alongside `surface`.
    pub activity_kind: Option<String>,
    /// Originating thread id when this run was spawned by another agent.
    pub activity_parent_thread_id: Option<String>,
    /// For `invoke_subagent`: the named subagent invoked.
    pub activity_subagent_name: Option<String>,
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
    let _run_guard = if options.track_activity {
        crate::agent_activity::start(crate::agent_activity::StartParams {
            thread_id,
            surface: options.surface.as_deref(),
            model: Some(setup.model.as_str()),
            kind: options.activity_kind.as_deref(),
            parent_thread_id: options.activity_parent_thread_id.as_deref(),
            subagent_name: options.activity_subagent_name.as_deref(),
        })
    } else {
        None
    };
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

                let outcome: std::result::Result<
                    StreamState,
                    (TurnError, bool, Option<StreamState>),
                > = match request_stream(
                    &setup.client,
                    &messages,
                    &setup.tools,
                    system_prompt,
                    cancel,
                )
                .await
                {
                    Ok(stream) => {
                        match consume_stream(stream, &messages, sender, cancel, &setup.model).await
                        {
                            Ok(state) => Ok(state),
                            Err((err, state)) => {
                                let can_retry = can_transparently_retry_stream(&state);
                                Err((err, can_retry, Some(state)))
                            }
                        }
                    }
                    Err(err) => Err((err, true, None)),
                };

                match outcome {
                    Ok(mut state) => {
                        // Commit the attempt: flush any buffered usage that
                        // hadn't yet hit a visible event (e.g. tool-only or
                        // pure-stop turns).
                        state.flush_pending_usage(sender);
                        break 'retry state;
                    }
                    Err((err, can_retry, state)) => {
                        let retry_err = match &err {
                            TurnError::Provider(p) if p.is_retryable() && can_retry => {
                                Some(p.clone())
                            }
                            _ => None,
                        };
                        let Some(retry_err) = retry_err else {
                            // Terminal failure (non-retryable, or retryable
                            // but already past the visible-content gate).
                            // Bill the partial attempt's buffered usage,
                            // then build a snapshot for provider failures
                            // so manual continue/retry resumes from a
                            // balanced thread state. Interruption errors
                            // already carry their own recovery messages
                            // inside the `TurnError::Interrupted` payload,
                            // so we never reuse the snapshot path for
                            // those.
                            let final_messages = match state {
                                Some(mut state) => {
                                    state.flush_pending_usage(sender);
                                    match &err {
                                        TurnError::Provider(_) => {
                                            build_provider_failed_messages(&messages, state.turn)
                                        }
                                        _ => messages.clone(),
                                    }
                                }
                                None => messages.clone(),
                            };
                            return Err((err, final_messages));
                        };
                        if attempt >= MAX_RETRIES {
                            // Max retries reached: same flush-then-snapshot
                            // discipline as the non-retryable branch.
                            let final_messages = match state {
                                Some(mut state) => {
                                    state.flush_pending_usage(sender);
                                    match &err {
                                        TurnError::Provider(_) => {
                                            build_provider_failed_messages(&messages, state.turn)
                                        }
                                        _ => messages.clone(),
                                    }
                                }
                                None => messages.clone(),
                            };
                            return Err((err, final_messages));
                        }
                        // Retryable continue: drop `state` (and its
                        // `pending_usage`) implicitly. The next attempt
                        // creates a fresh `StreamState`.
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
    client: Box<dyn StreamingProvider>,
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
    let provider_config = config.providers.get(provider);
    let provider_ctx = ProviderBuildContext::new(
        &selection.model,
        provider,
        max_tokens,
        config.max_tokens,
        thinking_level,
        options.text_verbosity,
        thread_id,
        options
            .service_tier
            .as_deref()
            .or(if provider_config.fast_mode {
                Some("priority")
            } else {
                None
            }),
        provider_config.effective_base_url(),
        provider_config.effective_api_key(),
        provider_config.effective_text_verbosity(),
        provider_config.websocket,
        if provider == ProviderKind::OpencodeGo {
            crate::models::ModelOption::find_by_provider_and_id("opencode-go", &selection.model)
                .and_then(|m| m.capabilities.api)
                .map(ToString::to_string)
        } else {
            None
        },
    );
    let client = provider.build_client(&provider_ctx)?;
    let tool_ctx = ToolContext::new(
        options
            .root
            .canonicalize()
            .unwrap_or_else(|_| options.root.clone()),
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

/// Resolves the tool list that would be sent to the LLM for the given
/// config / agent options / provider.
///
/// Mirrors the resolution the engine performs inside a real turn (including
/// `Invoke_Subagent` enrichment based on discovered subagents under
/// `options.root`). Exposed for external callers (e.g. the TUI
/// context-analyzer overlay) that need to size the tool block of an
/// outgoing request without actually starting a turn.
#[must_use]
pub fn resolve_active_tools(
    config: &Config,
    options: &AgentOptions,
    provider: ProviderKind,
) -> Vec<ToolDefinition> {
    resolve_tools(config, options, provider, &options.tool_config.registry)
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
        ToolSelection::All => tool_registry.definitions(),
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
    /// Buffered usage deltas accumulated since the last flush. Emitted as a
    /// single combined `AgentEvent::UsageUpdate` at commit boundaries
    /// (immediately before the first user-visible event of an attempt, on
    /// `consume_stream` EOF success, on user interruption, or on terminal
    /// failure in the retry loop). Discarded silently when an attempt is
    /// transparently retried, eliminating the double-counting that would
    /// otherwise occur when usage ticks arrive before any visible content.
    ///
    /// Invariant: every `handle_stream_event` arm that performs a visible
    /// `sender.send(...)` MUST call `state.flush_pending_usage(sender)`
    /// immediately beforehand. `consume_stream` and the retry loop also
    /// flush before returning on terminal paths.
    pending_usage: crate::providers::Usage,
    /// Whether any *visible* assistant content (text, reasoning, tool
    /// start/input/completion, persisted reasoning completion) has been
    /// emitted to the UI/persistence pipeline. Once true, transparent
    /// retries are no longer safe because the next attempt would duplicate
    /// content already shown to the user or appended to the transcript.
    ///
    /// Metadata-only emissions (usage updates) intentionally do **not**
    /// flip this flag, so a transport failure that arrives after a
    /// `MessageStart` / `MessageDelta` usage tick can still retry
    /// transparently. Buffered usage on the discarded attempt is dropped
    /// in `pending_usage`, so the retry contributes no leftover usage
    /// events.
    emitted_visible_content: bool,
}

impl StreamState {
    fn new(model: String) -> Self {
        Self {
            turn: AssistantTurnBuilder::new(model),
            stop_reason: None,
            usage_seen: crate::providers::Usage::default(),
            pending_usage: crate::providers::Usage::default(),
            emitted_visible_content: false,
        }
    }

    fn needs_tool_execution(&self) -> bool {
        self.stop_reason.as_deref() == Some("tool_use") && self.turn.has_tool_uses()
    }

    /// Emits one combined `AgentEvent::UsageUpdate` for any buffered usage
    /// since the last flush, then resets the buffer. No-op when the buffer
    /// is empty. Downstream consumers (TUI status bar, persistence, CLI
    /// exec) are additive, so a single combined event is equivalent to
    /// emitting each accumulated delta separately.
    fn flush_pending_usage(&mut self, sender: &EventSender) {
        if self.pending_usage.is_empty() {
            return;
        }
        let usage = std::mem::take(&mut self.pending_usage);
        sender.send(AgentEvent::UsageUpdate {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
        });
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
    client: &dyn StreamingProvider,
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
        result = client.stream_messages(messages, tools, system_prompt) => result,
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
            // User interruption is terminal, not a transparent retry: bill
            // the partial attempt's buffered usage before returning.
            state.flush_pending_usage(sender);
            let turn = std::mem::take(&mut state.turn);
            return Err((interrupted_turn_from_stream(prior_messages, turn), state));
        }
        let event = match timeout(STREAM_POLL_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(event))) => event,
            Ok(Some(Err(err))) => return Err((TurnError::Provider(err), state)),
            Ok(None) => {
                // EOF without an explicit `MessageCompleted`: defensive
                // flush so any buffered usage from a `MessageStart` /
                // `MessageDelta` tick that hadn't yet hit a visible event
                // still reaches downstream consumers on success.
                state.flush_pending_usage(sender);
                return Ok(state);
            }
            Err(_) => continue,
        };
        if let Err(err) = handle_stream_event(event, sender, &mut state) {
            return Err((err, state));
        }
    }
}

fn can_transparently_retry_stream(state: &StreamState) -> bool {
    !state.emitted_visible_content
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
            state.flush_pending_usage(sender);
            sender.send(AgentEvent::AssistantDelta { text });
            state.emitted_visible_content = true;
        }
        StreamEvent::ContentBlockStart {
            index,
            block_type: ContentBlockType::ToolUse,
            id,
            name,
            id_origin,
            ..
        } => {
            state.flush_pending_usage(sender);
            handle_tool_content_start(index, id, name, id_origin, sender, &mut state.turn);
            state.emitted_visible_content = true;
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
            if let Some(event) = build_input_json_delta(index, &partial_json, &mut state.turn) {
                state.flush_pending_usage(sender);
                sender.send(event);
                state.emitted_visible_content = true;
            }
        }
        StreamEvent::ReasoningDelta { index, reasoning } => {
            let event_opt = if let Some(tb) = state.turn.find_thinking_mut(index) {
                tb.text.push_str(&reasoning);
                if reasoning.is_empty() {
                    None
                } else {
                    tb.had_delta = true;
                    Some(AgentEvent::ReasoningDelta { text: reasoning })
                }
            } else {
                None
            };
            if let Some(event) = event_opt {
                state.flush_pending_usage(sender);
                sender.send(event);
                state.emitted_visible_content = true;
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
            let reasoning_event = build_reasoning_completion(&mut state.turn, index);
            let tool_event = build_tool_input_completion(&state.turn, index);
            if reasoning_event.is_some() || tool_event.is_some() {
                state.flush_pending_usage(sender);
                if let Some(event) = reasoning_event {
                    sender.send(event);
                }
                if let Some(event) = tool_event {
                    sender.send(event);
                }
                state.emitted_visible_content = true;
            }
            // Per-part Gemini signatures (text + tool_use) ride this channel.
            // Reasoning signatures still flow through `ReasoningSignatureDelta`
            // and were attached during `build_reasoning_completion` above, so
            // we ignore signatures on reasoning indices here.
            if let Some((sig_provider, sig)) = signature {
                attach_part_signature(&mut state.turn, index, sig_provider, sig);
            }
        }
        StreamEvent::MessageDelta { stop_reason, usage } => {
            state.stop_reason = stop_reason;
            // Usage updates are metadata-only: they accumulate into the
            // pending-usage buffer and only flush at commit boundaries, so
            // they do not block transparent retry and are discarded if the
            // attempt is retried before any visible content emits.
            buffer_message_delta_usage(state, usage);
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
            // `MessageStart` only emits a usage tick: metadata-only, buffered
            // until the first visible event commits the attempt.
            buffer_message_start_usage(state, &usage);
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

fn build_input_json_delta(
    index: usize,
    partial_json: &str,
    turn: &mut AssistantTurnBuilder,
) -> Option<AgentEvent> {
    let tu = turn.find_tool_use_mut(index)?;
    tu.input_json.push_str(partial_json);
    let delta = extract_partial_tool_input(&tu.name, &tu.input_json)?;
    if delta.is_empty() || delta.len() <= tu.input_preview_len {
        return None;
    }
    tu.input_preview_len = delta.len();
    Some(AgentEvent::ToolInputDelta {
        id: tu.id.clone(),
        name: tu.name.clone(),
        delta,
    })
}

/// Folds a `MessageDelta` usage tick into the stream state's pending-usage
/// buffer (additive on each field) and updates `usage_seen` so subsequent
/// sparse cumulative deltas compute correctly. Does not emit. The buffer
/// is flushed by `StreamState::flush_pending_usage` at commit boundaries.
fn buffer_message_delta_usage(
    state: &mut StreamState,
    usage: Option<crate::providers::UsageDelta>,
) {
    if let Some(u) = usage {
        let delta = u.incremental_from(&state.usage_seen);
        u.apply_to(&mut state.usage_seen);

        if !delta.is_empty() {
            accumulate_usage(&mut state.pending_usage, &delta);
        }
    }
}

/// Folds a `MessageStart` usage tick into the stream state's pending-usage
/// buffer (additive on each field) and seeds `usage_seen` with the cumulative
/// totals reported by the provider. Does not emit.
fn buffer_message_start_usage(state: &mut StreamState, usage: &crate::providers::Usage) {
    state.usage_seen = usage.clone();
    accumulate_usage(&mut state.pending_usage, usage);
}

fn accumulate_usage(target: &mut crate::providers::Usage, delta: &crate::providers::Usage) {
    target.input_tokens = target.input_tokens.saturating_add(delta.input_tokens);
    target.output_tokens = target.output_tokens.saturating_add(delta.output_tokens);
    target.cache_read_input_tokens = target
        .cache_read_input_tokens
        .saturating_add(delta.cache_read_input_tokens);
    target.cache_creation_input_tokens = target
        .cache_creation_input_tokens
        .saturating_add(delta.cache_creation_input_tokens);
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

fn build_reasoning_completion(turn: &mut AssistantTurnBuilder, index: usize) -> Option<AgentEvent> {
    let model = turn.model.clone();
    let tb = turn.find_thinking_mut(index)?;
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
    Some(AgentEvent::ReasoningCompleted { block })
}

fn build_tool_input_completion(turn: &AssistantTurnBuilder, index: usize) -> Option<AgentEvent> {
    let tu = turn.tool_uses().find(|t| t.index == index)?;
    // With fine-grained tool streaming (`eager_input_streaming: true`),
    // Anthropic may legitimately deliver partial/invalid JSON if a tool
    // call is truncated (e.g. `max_tokens` reached). Preserve the raw
    // payload under a sentinel field so persistence and downstream
    // diagnostics can still see what was emitted, instead of silently
    // collapsing to `{}` and discarding the evidence.
    let input: Value = serde_json::from_str(&tu.input_json)
        .unwrap_or_else(|_| malformed_tool_input_value(&tu.input_json));
    Some(AgentEvent::ToolInputCompleted {
        id: tu.id.clone(),
        name: tu.name.clone(),
        input,
    })
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

/// Builds a thread snapshot for a mid-stream **provider** failure.
///
/// Unlike `build_interrupted_messages` (used for user-initiated interrupts),
/// this MUST NOT synthesize `tool_result` blocks or any other recovery
/// scaffolding. Provider-failure recovery and interruption recovery are
/// distinct cases:
///
/// * Interruption owns the turn — the user explicitly stopped streaming, so
///   surfacing "Interrupted by user" tool results keeps the assistant↔tool
///   pairing balanced and accurately reflects what happened.
/// * Provider failure does not own the turn — the upstream simply died, and
///   no tool was actually canceled by the user. Emitting "Interrupted by
///   user" results would misrepresent the failure mode and could poison the
///   next turn's context.
///
/// Pending `ToolUse` blocks are also dropped: persisting them without
/// matching `tool_results` would unbalance the next provider request, so we
/// keep only fully safe partial output (text + reasoning). The user can then
/// manually continue or retry from that balanced snapshot.
fn build_provider_failed_messages(
    prior_messages: &[ChatMessage],
    turn: AssistantTurnBuilder,
) -> Vec<ChatMessage> {
    let finalized = turn.finalize();
    let mut messages = prior_messages.to_vec();

    let safe_blocks: Vec<ChatContentBlock> = finalized
        .blocks
        .into_iter()
        .filter(|block| !matches!(block, ChatContentBlock::ToolUse { .. }))
        .collect();

    if !safe_blocks.is_empty() {
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            phase: Some("commentary".to_string()),
            content: crate::providers::MessageContent::Blocks(safe_blocks),
        });
    }

    messages
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
    use crate::providers::anthropic::EffortLevel as AnthropicEffortLevel;
    use crate::providers::gemini::{GeminiClient, GeminiConfig};
    use crate::providers::{map_thinking_to_anthropic_effort, resolve_text_verbosity};

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
    fn anthropic_effort_opus_48_uses_one_to_one_shift_with_xhigh() {
        let m = "claude-opus-4-8";
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
    fn anthropic_effort_fable_5_uses_opus_48_mapping() {
        let m = "claude-fable-5";
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
    fn anthropic_effort_unknown_future_model_defaults_to_five_levels() {
        let m = "claude-opus-5";
        assert_eq!(
            map_thinking_to_anthropic_effort(ThinkingLevel::Minimal, m),
            Some(AnthropicEffortLevel::Low)
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
    fn build_tool_input_completion_preserves_malformed_json_under_sentinel() {
        // With eager_input_streaming GA, Anthropic may legitimately emit
        // truncated/invalid JSON when max_tokens is reached mid-tool.
        // We must not silently collapse the payload to `{}` and lose the
        // raw evidence — persistence and diagnostics need to see it.
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

        let evt = build_tool_input_completion(&turn, 0).expect("expected event");
        match evt {
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
    fn build_tool_input_completion_passes_through_valid_json() {
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

        let evt = build_tool_input_completion(&turn, 0).expect("expected event");
        match evt {
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
        // Buffered semantics: per-tick deltas accumulate into
        // `pending_usage` (no immediate emission) and flush as a single
        // combined `UsageUpdate` at commit boundaries. Verifies the
        // sparse-cumulative → incremental conversion still folds correctly:
        // start=(100,2,5,1), +delta(_,+8,_,_)=(0,8,0,0),
        // +delta(120,15,8,1)=(20,5,3,0). Sum: (120,15,8,1).
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

        // Nothing emitted yet — usage is buffered.
        assert!(rx.try_recv().is_err(), "usage must be buffered, not sent");
        assert_eq!(state.pending_usage.input_tokens, 120);
        assert_eq!(state.pending_usage.output_tokens, 15);
        assert_eq!(state.pending_usage.cache_read_input_tokens, 8);
        assert_eq!(state.pending_usage.cache_creation_input_tokens, 1);

        // Flush emits one combined event and resets the buffer.
        state.flush_pending_usage(&sender);
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
        assert_eq!(usage_updates, vec![(120, 15, 8, 1)]);
        assert!(state.pending_usage.is_empty());
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
        let enabled_tools: HashSet<String> = vec!["Todo_Write".to_string()].into_iter().collect();
        let tool_uses = vec![
            ToolUse {
                id: "tool1".to_string(),
                name: "Todo_Write".to_string(),
                input: serde_json::json!({
                    "todos": [
                        {"op": "add", "content": "Inspect codebase", "status": "in_progress"}
                    ]
                }),
                id_origin: zdx_types::IdOrigin::Synthesized,
                replay: None,
            },
            ToolUse {
                id: "tool2".to_string(),
                name: "Todo_Write".to_string(),
                input: serde_json::json!({
                    "todos": [
                        {"op": "update", "id": "todo-1", "status": "completed"},
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
            .expect("Todo_Write result should be valid JSON text");

        let todos = second
            .get("data")
            .and_then(|data| data.get("todos"))
            .and_then(|todos| todos.as_array())
            .expect("Todo_Write should return todos array");

        assert_eq!(todos.len(), 2);
        assert_eq!(
            todos[0].get("id").and_then(|id| id.as_str()),
            Some("todo-1")
        );
        assert_eq!(
            todos[0].get("status").and_then(|status| status.as_str()),
            Some("completed")
        );
        assert_eq!(
            todos[1].get("id").and_then(|id| id.as_str()),
            Some("todo-2")
        );
        assert_eq!(
            todos[1].get("status").and_then(|status| status.as_str()),
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

    /// Provider-failure recovery MUST NOT reuse the interruption-style
    /// synthesis: the user did not stop the stream, so synthesizing
    /// "Interrupted by user" `tool_results` would misrepresent the failure
    /// mode and could poison the next turn's context. Pending `ToolUse`
    /// blocks are also dropped because persisting them without matching
    /// `tool_results` would unbalance the next provider request. We keep
    /// only the safe partial output (text + reasoning) so the user can
    /// manually continue/retry from a balanced thread state.
    #[test]
    fn provider_failed_snapshot_preserves_text_and_reasoning_only() {
        use crate::providers::{ChatContentBlock, MessageContent};

        let prior_messages = vec![ChatMessage::user("analyze the repo")];
        let mut turn = AssistantTurnBuilder::new(String::new());
        turn.push_reasoning(ThinkingBuilder {
            index: 0,
            text: "Inspecting the project first.".to_string(),
            signature: String::new(),
            signature_provider: None,
            replay: Some(ReplayToken::Anthropic {
                signature: "sig-r".to_string(),
            }),
            had_delta: true,
        });
        turn.ensure_text_part_mut(1).text = "Found the loop.".to_string();
        // Pending tool use that never received a tool_result; the
        // provider-failure snapshot MUST drop this to keep the
        // assistant↔tool_result pairing balanced.
        turn.push_tool_use(ToolUseBuilder {
            index: 2,
            id: "tool_pending".to_string(),
            name: "read".to_string(),
            input_json: r#"{"file_path":"src/main.rs"}"#.to_string(),
            input_preview_len: 0,
            id_origin: zdx_types::IdOrigin::Synthesized,
            replay: None,
        });

        let messages = build_provider_failed_messages(&prior_messages, turn);

        // Expect exactly: prior user message + one assistant snapshot.
        // No synthetic tool_result message should be appended.
        assert_eq!(messages.len(), 2, "messages: {messages:#?}");
        assert_eq!(messages[0], prior_messages[0]);

        let ChatMessage {
            role,
            phase,
            content,
        } = &messages[1];
        assert_eq!(role, "assistant");
        assert_eq!(phase.as_deref(), Some("commentary"));
        let MessageContent::Blocks(blocks) = content else {
            panic!("expected blocks content, got {content:?}");
        };

        // Reasoning + text are preserved.
        assert!(blocks.iter().any(|b| matches!(
            b,
            ChatContentBlock::Reasoning(ReasoningBlock {
                text: Some(text),
                replay: Some(ReplayToken::Anthropic { signature }),
            }) if text == "Inspecting the project first." && signature == "sig-r"
        )));
        assert!(blocks.iter().any(|b| matches!(
            b,
            ChatContentBlock::Text { text, .. } if text == "Found the loop."
        )));

        // Pending tool use is dropped — no ToolUse block AND no synthetic
        // ToolResult message.
        assert!(
            !blocks
                .iter()
                .any(|b| matches!(b, ChatContentBlock::ToolUse { .. })),
            "pending tool_use must be dropped from provider-failure snapshot",
        );
        assert!(
            !messages.iter().any(|m| matches!(
                &m.content,
                MessageContent::Blocks(bs)
                    if bs.iter().any(|b| matches!(b, ChatContentBlock::ToolResult(_)))
            )),
            "provider-failure snapshot must NOT synthesize tool_results",
        );
    }

    /// When the assistant emitted no visible content yet (only metadata),
    /// the provider-failure snapshot reduces to the prior messages with no
    /// extra assistant block. This keeps the failed-turn payload empty for
    /// pre-output failures so `emit_turn_error` (not the
    /// `_with_messages` variant) handles them.
    #[test]
    fn provider_failed_snapshot_is_empty_when_no_visible_blocks() {
        let prior = vec![ChatMessage::user("hi")];
        let turn = AssistantTurnBuilder::new(String::new());
        let messages = build_provider_failed_messages(&prior, turn);
        assert_eq!(messages, prior);
    }

    /// Even when a `ToolUse` block was fully assembled (parseable JSON,
    /// would have been executable) but the provider fails before
    /// `MessageCompleted`, the snapshot MUST still drop it. The tool was
    /// never executed, so there is no honest `tool_result` to pair it with;
    /// preserving the orphan tool call would unbalance the next provider
    /// request and synthesizing a fake result would misrepresent the
    /// failure mode (see `build_provider_failed_messages` doc comment).
    #[test]
    fn provider_failed_snapshot_drops_completed_tool_use_without_result() {
        use crate::providers::{ChatContentBlock, MessageContent};

        let prior = vec![ChatMessage::user("read it")];
        let mut turn = AssistantTurnBuilder::new(String::new());
        turn.ensure_text_part_mut(0).text = "Reading…".to_string();
        // Complete, parseable tool input — `finalize()` would emit it as a
        // proper `ToolUse` block. Provider died before `MessageCompleted`,
        // so it never reached `process_tool_turn`.
        turn.push_tool_use(ToolUseBuilder {
            index: 1,
            id: "tool_done".to_string(),
            name: "read".to_string(),
            input_json: r#"{"file_path":"src/main.rs"}"#.to_string(),
            input_preview_len: 0,
            id_origin: zdx_types::IdOrigin::Synthesized,
            replay: None,
        });

        let messages = build_provider_failed_messages(&prior, turn);

        assert_eq!(messages.len(), 2, "messages: {messages:#?}");
        let MessageContent::Blocks(blocks) = &messages[1].content else {
            panic!("expected blocks content, got {:?}", messages[1].content);
        };
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, ChatContentBlock::Text { text, .. } if text == "Reading…")),
            "text content must survive",
        );
        assert!(
            !blocks
                .iter()
                .any(|b| matches!(b, ChatContentBlock::ToolUse { .. })),
            "fully-formed but unexecuted tool_use must still be dropped",
        );
        // No synthetic tool_result message either.
        assert!(
            !messages.iter().any(|m| matches!(
                &m.content,
                MessageContent::Blocks(bs)
                    if bs.iter().any(|b| matches!(b, ChatContentBlock::ToolResult(_)))
            )),
            "provider-failure snapshot must NOT synthesize tool_results",
        );
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

    /// Transparent retries are only safe before the stream emits any visible
    /// assistant content. Metadata-only emissions (usage ticks) intentionally
    /// do not flip the gate; see `test_consume_stream_keeps_usage_only_retry_safe`.
    #[test]
    fn test_can_transparently_retry_stream_requires_no_emitted_events() {
        let mut state = StreamState::new(String::new());
        assert!(can_transparently_retry_stream(&state));

        state.emitted_visible_content = true;
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

    /// SSE transport-level failures emitted by the parser before any stream
    /// events have been forwarded MUST satisfy the engine's transparent-retry
    /// gate so the unified retry loop can recover. This pins the contract that
    /// parser-side transport errors flow through `is_retryable` + the retry
    /// gate together.
    #[tokio::test]
    async fn test_consume_stream_treats_sse_transport_error_as_retryable() {
        use futures_util::stream;

        use crate::providers::ProviderErrorKind;

        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);

        // Mirrors the message shape produced by the OpenAI/Anthropic/Gemini
        // SSE parsers when the underlying byte stream errors mid-poll.
        let transport_err = ProviderError::new(
            ProviderErrorKind::Timeout,
            "SSE stream network error: connection reset by peer",
        );
        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![Err(transport_err)];
        let provider_stream: ProviderStream = Box::pin(stream::iter(events));

        let result = consume_stream(provider_stream, &[], &sender, None, "").await;
        let Err((err, state)) = result else {
            panic!("stream should error out");
        };
        assert!(matches!(
            &err,
            TurnError::Provider(p)
                if p.is_retryable()
                    && p.kind == ProviderErrorKind::Timeout
                    && p.message.contains("network error")
        ));
        assert!(
            can_transparently_retry_stream(&state),
            "transport error before any emitted events must remain retry-safe"
        );
    }

    /// `MessageStart` only emits a usage tick, which is metadata-only: the UI
    /// status bar accumulates token counters but no transcript content is
    /// appended. A transport failure right after `MessageStart` MUST therefore
    /// remain retry-safe so the engine can transparently retry the turn.
    #[tokio::test]
    async fn test_consume_stream_keeps_usage_only_retry_safe() {
        use futures_util::stream;

        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 12,
                    ..crate::providers::Usage::default()
                },
            }),
            Ok(StreamEvent::MessageDelta {
                stop_reason: None,
                usage: Some(crate::providers::UsageDelta {
                    output_tokens: Some(3),
                    ..crate::providers::UsageDelta::default()
                }),
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
            can_transparently_retry_stream(&state),
            "metadata-only usage updates must not block transparent retry"
        );
    }

    /// `flush_pending_usage` emits ONE combined `UsageUpdate` carrying the
    /// accumulated buffer and resets `pending_usage` to default. Downstream
    /// consumers are additive, so a single combined event is equivalent to
    /// emitting each per-tick delta separately.
    #[test]
    fn flush_pending_usage_emits_combined_event_then_resets() {
        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let mut state = StreamState::new(String::new());
        state.pending_usage = crate::providers::Usage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_input_tokens: 3,
            cache_creation_input_tokens: 2,
        };

        state.flush_pending_usage(&sender);

        let evt = rx.try_recv().expect("expected a flushed UsageUpdate");
        match &*evt {
            AgentEvent::UsageUpdate {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            } => {
                assert_eq!(*input_tokens, 11);
                assert_eq!(*output_tokens, 7);
                assert_eq!(*cache_read_input_tokens, 3);
                assert_eq!(*cache_creation_input_tokens, 2);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "exactly one event should flush");
        assert!(
            state.pending_usage.is_empty(),
            "buffer must reset after flush"
        );
    }

    #[test]
    fn flush_pending_usage_is_noop_when_empty() {
        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let mut state = StreamState::new(String::new());

        state.flush_pending_usage(&sender);

        assert!(
            rx.try_recv().is_err(),
            "no event should be emitted when the buffer is empty"
        );
    }

    /// `MessageStart` + `MessageDelta` deltas BEFORE any visible content
    /// accumulate into `pending_usage` and never reach the channel.
    /// Combined with the existing retry-gate test, this pins the discard
    /// invariant that eliminates double-counting on transparent retry.
    #[tokio::test]
    async fn usage_buffer_accumulates_message_start_plus_message_delta_pre_content() {
        use futures_util::stream;

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 10,
                    ..crate::providers::Usage::default()
                },
            }),
            Ok(StreamEvent::MessageDelta {
                stop_reason: None,
                usage: Some(crate::providers::UsageDelta {
                    output_tokens: Some(5),
                    ..crate::providers::UsageDelta::default()
                }),
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

        assert_eq!(state.pending_usage.input_tokens, 10);
        assert_eq!(state.pending_usage.output_tokens, 5);
        assert!(
            can_transparently_retry_stream(&state),
            "buffered usage must not flip the retry gate"
        );
        // No `UsageUpdate` should have been emitted: the buffer is dropped
        // when the next attempt creates a fresh `StreamState`.
        let leaked: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|e| matches!(&**e, AgentEvent::UsageUpdate { .. }))
            .collect();
        assert!(
            leaked.is_empty(),
            "no UsageUpdate must leak from a discarded attempt"
        );
    }

    /// Buffered usage flushes immediately before the first user-visible
    /// `AssistantDelta`, preserving event ordering for downstream consumers
    /// that rely on usage arriving with the streaming delta.
    #[tokio::test]
    async fn usage_flushed_before_first_assistant_delta() {
        use futures_util::stream;

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 10,
                    ..crate::providers::Usage::default()
                },
            }),
            Ok(StreamEvent::TextDelta {
                index: 0,
                text: "hi".to_string(),
            }),
        ];
        let provider_stream: ProviderStream = Box::pin(stream::iter(events));

        let _ = consume_stream(provider_stream, &[], &sender, None, "").await;

        let usage_evt = rx.try_recv().expect("expected UsageUpdate first");
        assert!(
            matches!(
                &*usage_evt,
                AgentEvent::UsageUpdate {
                    input_tokens: 10,
                    ..
                }
            ),
            "first event must be the flushed UsageUpdate, got {usage_evt:?}"
        );
        let delta_evt = rx.try_recv().expect("expected AssistantDelta next");
        assert!(
            matches!(
                &*delta_evt,
                AgentEvent::AssistantDelta { text } if text == "hi"
            ),
            "second event must be the AssistantDelta, got {delta_evt:?}"
        );
    }

    /// Buffered usage flushes immediately before the first
    /// `ToolRequested` event triggered by a `tool_use` `ContentBlockStart`.
    #[tokio::test]
    async fn usage_flushed_before_first_tool_requested() {
        use futures_util::stream;

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 7,
                    ..crate::providers::Usage::default()
                },
            }),
            Ok(StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::ToolUse,
                id: Some("toolu_a".to_string()),
                name: Some("read".to_string()),
                data: None,
                id_origin: Some(zdx_types::IdOrigin::Real),
            }),
        ];
        let provider_stream: ProviderStream = Box::pin(stream::iter(events));

        let _ = consume_stream(provider_stream, &[], &sender, None, "").await;

        let usage_evt = rx.try_recv().expect("expected UsageUpdate first");
        assert!(
            matches!(
                &*usage_evt,
                AgentEvent::UsageUpdate {
                    input_tokens: 7,
                    ..
                }
            ),
            "first event must be the flushed UsageUpdate, got {usage_evt:?}"
        );
        // Drain remaining; ensure ToolRequested is among them and arrives
        // after the usage event.
        let saw_tool_requested = std::iter::from_fn(|| rx.try_recv().ok())
            .any(|e| matches!(&*e, AgentEvent::ToolRequested { .. }));
        assert!(
            saw_tool_requested,
            "expected a ToolRequested event after the flushed UsageUpdate"
        );
    }

    /// Buffered usage flushes immediately before the first `ToolInputDelta`
    /// — only when the helper actually has a non-empty preview to emit.
    #[tokio::test]
    async fn usage_flushed_before_first_tool_input_delta() {
        use futures_util::stream;

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 9,
                    ..crate::providers::Usage::default()
                },
            }),
            Ok(StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::ToolUse,
                id: Some("toolu_w".to_string()),
                name: Some("write".to_string()),
                data: None,
                id_origin: Some(zdx_types::IdOrigin::Real),
            }),
            Ok(StreamEvent::InputJsonDelta {
                index: 0,
                partial_json: "{\"path\":\"a.txt\",\"content\":\"hi\"".to_string(),
            }),
        ];
        let provider_stream: ProviderStream = Box::pin(stream::iter(events));

        let _ = consume_stream(provider_stream, &[], &sender, None, "").await;

        // First event must be the flushed UsageUpdate (triggered by the
        // ToolUse start arm). The subsequent InputJsonDelta does not need
        // its own flush — the buffer is already empty.
        let first = rx.try_recv().expect("expected first event");
        assert!(
            matches!(
                &*first,
                AgentEvent::UsageUpdate {
                    input_tokens: 9,
                    ..
                }
            ),
            "first event must be the flushed UsageUpdate, got {first:?}"
        );
        // Drain and confirm a ToolInputDelta arrived with the partial preview.
        let saw_input_delta = std::iter::from_fn(|| rx.try_recv().ok()).any(|e| {
            matches!(
                &*e,
                AgentEvent::ToolInputDelta { name, delta, .. }
                    if name == "write" && delta == "hi"
            )
        });
        assert!(
            saw_input_delta,
            "expected ToolInputDelta carrying the write tool's content preview"
        );
    }

    /// Buffered usage flushes immediately before the first non-empty
    /// `ReasoningDelta`.
    #[tokio::test]
    async fn usage_flushed_before_first_reasoning_delta() {
        use futures_util::stream;

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 4,
                    ..crate::providers::Usage::default()
                },
            }),
            Ok(StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::Reasoning,
                id: None,
                name: None,
                data: None,
                id_origin: None,
            }),
            Ok(StreamEvent::ReasoningDelta {
                index: 0,
                reasoning: "thinking".to_string(),
            }),
        ];
        let provider_stream: ProviderStream = Box::pin(stream::iter(events));

        let _ = consume_stream(provider_stream, &[], &sender, None, "").await;

        let first = rx.try_recv().expect("expected first event");
        assert!(
            matches!(
                &*first,
                AgentEvent::UsageUpdate {
                    input_tokens: 4,
                    ..
                }
            ),
            "first event must be the flushed UsageUpdate, got {first:?}"
        );
        let second = rx.try_recv().expect("expected ReasoningDelta next");
        assert!(
            matches!(&*second, AgentEvent::ReasoningDelta { text } if text == "thinking"),
            "second event must be the ReasoningDelta, got {second:?}"
        );
    }

    /// Buffered usage flushes immediately before the first
    /// `ReasoningCompleted` produced by `build_reasoning_completion` on a
    /// `ContentBlockCompleted`.
    #[tokio::test]
    async fn usage_flushed_before_first_reasoning_completed() {
        use futures_util::stream;

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 6,
                    ..crate::providers::Usage::default()
                },
            }),
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
        ];
        let provider_stream: ProviderStream = Box::pin(stream::iter(events));

        let _ = consume_stream(provider_stream, &[], &sender, None, "").await;

        let first = rx.try_recv().expect("expected first event");
        assert!(
            matches!(
                &*first,
                AgentEvent::UsageUpdate {
                    input_tokens: 6,
                    ..
                }
            ),
            "first event must be the flushed UsageUpdate, got {first:?}"
        );
        let second = rx.try_recv().expect("expected ReasoningCompleted next");
        assert!(
            matches!(&*second, AgentEvent::ReasoningCompleted { .. }),
            "second event must be the ReasoningCompleted, got {second:?}"
        );
    }

    /// An empty `ReasoningDelta` MUST NOT flush buffered usage and MUST
    /// NOT flip the retry gate. The TUI already filters empty deltas at
    /// render time, so flipping the gate on a no-op event was a small
    /// Slice-2 bug — corrected here.
    #[tokio::test]
    async fn empty_reasoning_delta_does_not_flush_or_flip_gate() {
        use futures_util::stream;

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 5,
                    ..crate::providers::Usage::default()
                },
            }),
            Ok(StreamEvent::ContentBlockStart {
                index: 0,
                block_type: ContentBlockType::Reasoning,
                id: None,
                name: None,
                data: None,
                id_origin: None,
            }),
            Ok(StreamEvent::ReasoningDelta {
                index: 0,
                reasoning: String::new(),
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

        assert_eq!(state.pending_usage.input_tokens, 5);
        assert!(
            can_transparently_retry_stream(&state),
            "empty reasoning delta must not flip the retry gate"
        );
        let leaked: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|e| matches!(&**e, AgentEvent::UsageUpdate { .. }))
            .collect();
        assert!(
            leaked.is_empty(),
            "buffered usage must not flush on an empty reasoning delta"
        );
    }

    /// EOF without an explicit `MessageCompleted` MUST still flush buffered
    /// usage on the success path so TUI/persistence consumers see the final
    /// counter values.
    #[tokio::test]
    async fn usage_flushed_on_eof_success_without_message_completed() {
        use futures_util::stream;

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 3,
                    ..crate::providers::Usage::default()
                },
            }),
            Ok(StreamEvent::MessageDelta {
                stop_reason: Some("end_turn".to_string()),
                usage: Some(crate::providers::UsageDelta {
                    output_tokens: Some(2),
                    ..crate::providers::UsageDelta::default()
                }),
            }),
        ];
        let provider_stream: ProviderStream = Box::pin(stream::iter(events));

        let result = consume_stream(provider_stream, &[], &sender, None, "").await;
        let Ok(state) = result else {
            panic!("stream should succeed on EOF");
        };
        assert!(state.pending_usage.is_empty(), "buffer must be flushed");

        let evt = rx.try_recv().expect("expected a flushed UsageUpdate");
        match &*evt {
            AgentEvent::UsageUpdate {
                input_tokens,
                output_tokens,
                ..
            } => {
                assert_eq!(*input_tokens, 3);
                assert_eq!(*output_tokens, 2);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    /// User interruption is terminal (not transparent retry): the partial
    /// attempt's buffered usage MUST flush before `consume_stream` returns
    /// the `TurnError::Interrupted`.
    #[tokio::test]
    async fn usage_flushed_on_user_interruption_pre_content() {
        use futures_util::stream::{self, StreamExt};

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let cancel = CancellationToken::new();

        // Yield `MessageStart` (gets buffered), then stay pending so the
        // loop hits `STREAM_POLL_TIMEOUT` and re-checks the cancel token.
        let provider_stream: ProviderStream = Box::pin(
            stream::iter(vec![Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 8,
                    ..crate::providers::Usage::default()
                },
            })])
            .chain(stream::pending()),
        );

        // Cancel mid-flight, after `MessageStart` has been processed.
        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            canceller.cancel();
        });

        let result = consume_stream(provider_stream, &[], &sender, Some(&cancel), "").await;
        let Err((err, state)) = result else {
            panic!("stream should be interrupted");
        };
        assert!(matches!(err, TurnError::Interrupted { .. }));
        assert!(
            state.pending_usage.is_empty(),
            "buffer must be flushed before interruption return"
        );

        let evt = rx.try_recv().expect("expected a flushed UsageUpdate");
        assert!(
            matches!(
                &*evt,
                AgentEvent::UsageUpdate {
                    input_tokens: 8,
                    ..
                }
            ),
            "user interruption must bill the partial attempt, got {evt:?}"
        );
    }

    /// Simulates a transparent retry at the `consume_stream` layer:
    /// the first attempt buffers usage and fails retryably (state is
    /// discarded by the retry loop). The second attempt streams text and
    /// commits its own usage. End-to-end, the channel observes exactly ONE
    /// `UsageUpdate` — the discarded attempt left no leak.
    #[tokio::test]
    async fn usage_emitted_once_after_transparent_retry_success() {
        use futures_util::stream;

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);

        // Attempt 1: MessageStart + retryable error. State is dropped.
        let attempt1: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 10,
                    ..crate::providers::Usage::default()
                },
            }),
            Err(ProviderError::api_error(
                "overloaded_error",
                "API is temporarily overloaded",
            )),
        ];
        let s1: ProviderStream = Box::pin(stream::iter(attempt1));
        let r1 = consume_stream(s1, &[], &sender, None, "").await;
        let Err((_, discarded)) = r1 else {
            panic!("attempt 1 should fail");
        };
        assert!(
            can_transparently_retry_stream(&discarded),
            "attempt 1 must be retry-safe"
        );
        // Drop discarded state: simulates the retry loop's behavior.
        drop(discarded);

        // Attempt 2: succeeds with the same MessageStart + text + EOF.
        let attempt2: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage {
                    input_tokens: 10,
                    ..crate::providers::Usage::default()
                },
            }),
            Ok(StreamEvent::TextDelta {
                index: 0,
                text: "ok".to_string(),
            }),
        ];
        let s2: ProviderStream = Box::pin(stream::iter(attempt2));
        let r2 = consume_stream(s2, &[], &sender, None, "").await;
        assert!(r2.is_ok(), "attempt 2 should succeed");

        // Drain rx and pin the strict ordering: exactly one UsageUpdate
        // (from attempt 2) emitted BEFORE the AssistantDelta, and nothing
        // else carrying usage data.
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        let usage_positions: Vec<_> = events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match &**e {
                AgentEvent::UsageUpdate { input_tokens, .. } => Some((i, *input_tokens)),
                _ => None,
            })
            .collect();
        assert_eq!(
            usage_positions.len(),
            1,
            "exactly one UsageUpdate must reach the channel; got {usage_positions:?}"
        );
        let (usage_idx, usage_input) = usage_positions[0];
        assert_eq!(usage_input, 10, "UsageUpdate must carry attempt 2's input");
        let delta_idx = events
            .iter()
            .position(|e| matches!(&**e, AgentEvent::AssistantDelta { text } if text == "ok"))
            .expect("attempt 2's AssistantDelta must reach the channel");
        assert!(
            usage_idx < delta_idx,
            "UsageUpdate (idx {usage_idx}) must precede AssistantDelta (idx {delta_idx})"
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

    /// Visible assistant text MUST block transparent retry: the user has
    /// already seen the partial output, so a second attempt would duplicate
    /// content. This pins the contract complementary to
    /// `test_consume_stream_keeps_usage_only_retry_safe`.
    #[tokio::test]
    async fn test_consume_stream_marks_text_delta_as_retry_unsafe() {
        use futures_util::stream;

        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![
            Ok(StreamEvent::MessageStart {
                model: "claude-test".to_string(),
                usage: crate::providers::Usage::default(),
            }),
            Ok(StreamEvent::TextDelta {
                index: 0,
                text: "Hello".to_string(),
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
            "visible assistant text must block transparent retry"
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
            client: Box::new(GeminiClient::new(GeminiConfig {
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
