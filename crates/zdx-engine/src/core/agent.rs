//! Agent module for UI-agnostic execution.
//!
//! The agent drives the provider + tool loop and emits `AgentEvent`s
//! via async channels. No direct stdout/stderr writes occur in this module.

use std::collections::HashSet;
use std::future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use crate::config::{Config, CustomProviderApi, TextVerbosity, ThinkingLevel};
use crate::core::events::{AgentEvent, ErrorKind, NoticeKind, ToolOutput, TurnStatus};
use crate::core::interrupt::{self, InterruptedError};
use crate::providers::{
    ChatContentBlock, ChatMessage, ContentBlockType, MessageContent, ProviderBuildContext,
    ProviderError, ProviderKind, ProviderStream, ReasoningBlock, ReplayToken, StreamEvent,
    StreamingProvider, anthropic, resolve_provider,
};
use crate::subagents;
use crate::tools::{
    BACKGROUND_MANAGEMENT_TOOLS, BACKGROUND_STARTING_TOOLS, BackgroundExecution, ToolContext,
    ToolDefinition, ToolRegistry, ToolResult, ToolResultBlock, ToolResultContent, ToolSet,
};

/// Options for agent execution.
#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// Stable identity for a threadless multi-turn conversation (`OpenCode` routing).
    pub conversation_id: Option<String>,
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
    /// Logical role for this run, e.g. `"chat"`, `"exec"`, `"telegram"`,
    /// `"subagent"`. Surfaced in the active-agents registry alongside `surface`.
    pub activity_kind: Option<String>,
    /// Originating thread id when this run was spawned by another agent.
    pub activity_parent_thread_id: Option<String>,
    /// For `invoke_subagent`: the named subagent invoked.
    pub activity_subagent_name: Option<String>,
    /// Per-run cap on a single tool call, set by a job's `timeout_secs`
    /// frontmatter. `None` (the default everywhere else) means tool calls are
    /// not bounded by the runner. This is the only deadline that kills a `Bash`
    /// command, and the model cannot raise, remove, or reach it: the `Bash`
    /// tool's own `timeout_secs` argument only bounds the foreground wait.
    pub tool_timeout: Option<Duration>,
    /// Steers for this run, taken at safe points (after a tool round, before
    /// the next model request). `None`: the run takes no steers.
    pub steer: Option<crate::core::steer::SteerSource>,
    /// The thread's run lease, taken by the caller before it recorded the
    /// user message. A turn on a thread without its lease takes one itself,
    /// and is refused when another run holds it.
    pub run_lease: Option<std::sync::Arc<crate::core::thread_lease::ThreadRunLease>>,
}

/// Tool configuration for agent execution.
#[derive(Debug, Clone)]
pub struct ToolConfig {
    pub registry: ToolRegistry,
    pub selection: ToolSelection,
    /// Restricts which subagents `invoke_subagent` may reach. `None` is
    /// unrestricted. Set from a persistent profile's `allowed_subagents`.
    pub allowed_subagents: Option<Vec<String>>,
    /// Whether this run may use background execution in `Bash`. On by
    /// default; off for a subagent unless its definition opts in. The run's
    /// [`BackgroundExecution`] is resolved from it.
    pub background_commands: bool,
    /// Skills the `skill` and `skill_exec` tools may reach. The run's
    /// configured catalog by default; a subagent or profile that declares
    /// skills narrows it to those.
    pub skill_scope: crate::skills::SkillScope,
}

impl ToolConfig {
    pub fn new(registry: ToolRegistry, selection: ToolSelection) -> Self {
        Self {
            registry,
            selection,
            allowed_subagents: None,
            background_commands: true,
            skill_scope: crate::skills::SkillScope::default(),
        }
    }

    #[must_use]
    pub fn with_skill_scope(mut self, scope: crate::skills::SkillScope) -> Self {
        self.skill_scope = scope;
        self
    }

    #[must_use]
    pub fn with_allowed_subagents(mut self, allowed: Option<Vec<String>>) -> Self {
        self.allowed_subagents = allowed;
        self
    }

    #[must_use]
    pub fn with_background_commands(mut self, enabled: bool) -> Self {
        self.background_commands = enabled;
        self
    }
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self::new(ToolRegistry::builtins(), ToolSelection::default())
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
///
/// When bound to an active-run guard via [`EventSender::with_activity`], it
/// derives the run's coarse phase (`waiting`/`thinking`/`answering`/
/// `retrying`) from the events flowing through it and records transitions in
/// the run marker. Phase updates are no-ops when unchanged, so per-delta
/// calls stay cheap.
#[derive(Clone)]
pub struct EventSender {
    tx: AgentEventTx,
    activity: Option<Arc<crate::agent_activity::RunGuard>>,
}

impl EventSender {
    /// Creates a new `EventSender` wrapping the given channel sender.
    pub fn new(tx: AgentEventTx) -> Self {
        Self { tx, activity: None }
    }

    /// Returns a sender that also records run-phase transitions in `guard`.
    #[must_use]
    pub fn with_activity(mut self, guard: Option<Arc<crate::agent_activity::RunGuard>>) -> Self {
        self.activity = guard;
        self
    }

    /// Sends an event. Never blocks; ignored silently if the receiver has dropped.
    pub fn send(&self, ev: AgentEvent) {
        if let Some(guard) = &self.activity {
            guard.touch_activity();
            match &ev {
                AgentEvent::ReasoningDelta { .. } => guard.set_phase("thinking"),
                AgentEvent::AssistantDelta { .. } => guard.set_phase("answering"),
                AgentEvent::ProviderRetry { .. } => guard.set_phase("retrying"),
                AgentEvent::ToolOutputDelta { id, chunk } => guard.append_tool_output(id, chunk),
                // Emitted after a tool round flushes, right before the next
                // model request goes out.
                AgentEvent::TurnCheckpoint { .. } => guard.set_phase("waiting"),
                _ => {}
            }
        }
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
                AssistantPart::Reasoning(mut tb) => {
                    if tb.text.is_empty() && tb.replay.is_none() {
                        // Drop empty reasoning placeholders that never received
                        // any deltas or replay metadata; they would otherwise
                        // serialize as `{ "type": "reasoning" }` with no body.
                        continue;
                    }
                    if tb.text.len() > MAX_PERSISTED_REASONING_CHARS {
                        // A runaway reasoning block is persisted truncated so
                        // the thread stays loadable. The replay token goes with
                        // the dropped text: a truncated block no longer matches
                        // its signature, and replaying the mismatch would be
                        // rejected upstream.
                        tb.text = truncate_middle(&tb.text, MAX_PERSISTED_REASONING_CHARS);
                        tb.replay = None;
                        tb.signature.clear();
                        tb.signature_provider = None;
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
    /// Short, bounded description for logging (never the full payload).
    fn log_summary(&self) -> String {
        let raw = match self {
            Self::Interrupted { .. } => "interrupted".to_string(),
            Self::Provider(p) => format!("provider: {}", p.message),
            Self::Parse { message, .. } => format!("parse: {message}"),
            Self::Internal(err) => format!("internal: {err}"),
        };
        zdx_types::log_field(&raw, 200)
    }

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

/// Keeps the head and tail of an over-long string, replacing the middle with a
/// marker. Both ends are preserved because a degenerate block's opening and
/// closing lines are what tell a reader (or a later turn) what happened.
fn truncate_middle(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let half = max_len / 2;
    let head_end = floor_char_boundary(s, half);
    let tail_start = floor_char_boundary(s, s.len() - half);
    format!(
        "{}\n\n... ({} bytes truncated) ...\n\n{}",
        &s[..head_end],
        s.len() - head_end - (s.len() - tail_start),
        &s[tail_start..]
    )
}

/// Detects a text tail made of the same few short lines repeated.
///
/// A model that has degenerated into filler emits lines like `Let me run.`
/// over and over; a genuine deliberation tail has either longer lines or more
/// variety than the window allows.
fn reasoning_repeats(text: &str) -> bool {
    repeats_short_lines(text, REASONING_REPETITION)
}

/// Shape of a degenerate tail: a trailing window containing nothing but short
/// lines, nearly all of them the same one.
#[derive(Debug, Clone, Copy)]
struct RepetitionThresholds {
    /// Trailing characters examined.
    window_chars: usize,
    /// Longest line still considered filler.
    max_line_chars: usize,
    /// Lines required before a window can be judged.
    min_lines: usize,
    /// Distinct lines allowed in the window.
    max_distinct_lines: usize,
    /// Occurrences of the most frequent line required to call it a loop.
    min_top_repeats: usize,
}

/// Reasoning thresholds. Validated against every reasoning block in the 400
/// most recent real threads (4,678 blocks, zero false positives) and all three
/// known runaway turns (all caught).
const REASONING_REPETITION: RepetitionThresholds = RepetitionThresholds {
    window_chars: 2_000,
    max_line_chars: 40,
    min_lines: 8,
    max_distinct_lines: 6,
    min_top_repeats: 4,
};

/// Answer-text thresholds, deliberately stricter than the reasoning ones: an
/// aborted answer is user-visible, and legitimately repetitive output (tables,
/// similar log lines, generated code) is common. `min_top_repeats` is what
/// keeps those safe — a table or a log dump has no line repeated ten times —
/// while any long line or real variety in the window clears the check.
/// Validated against every assistant message in the 400 most recent real
/// threads (2,163 messages): the only flagged one is a genuine 1.3 MB filler
/// loop.
const TEXT_REPETITION: RepetitionThresholds = RepetitionThresholds {
    window_chars: 4_000,
    max_line_chars: 40,
    min_lines: 20,
    max_distinct_lines: 6,
    min_top_repeats: 10,
};

/// Returns whether `text`'s tail is the same few short lines repeated.
fn repeats_short_lines(text: &str, thresholds: RepetitionThresholds) -> bool {
    if text.len() < thresholds.window_chars {
        return false;
    }
    let start = floor_char_boundary(text, text.len() - thresholds.window_chars);
    // The window can begin mid-line; that fragment is not a real line and
    // would otherwise read as one more distinct entry.
    let window = match text[start..].find('\n') {
        Some(offset) => &text[start + offset + 1..],
        None => "",
    };
    let mut lines: Vec<&str> = Vec::new();
    for line in window.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.len() > thresholds.max_line_chars {
            return false;
        }
        lines.push(line);
    }
    if lines.len() < thresholds.min_lines {
        return false;
    }
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for line in &lines {
        *counts.entry(line).or_insert(0) += 1;
    }
    counts.len() <= thresholds.max_distinct_lines
        && counts.values().copied().max().unwrap_or(0) >= thresholds.min_top_repeats
}

/// Largest byte index <= `index` that is a UTF-8 character boundary.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    let mut index = index.min(s.len());
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Counts identical tool calls within a no-progress window, so a model that
/// keeps re-issuing the same call and getting the same result back can be
/// stopped instead of grinding until the turn budget runs out.
///
/// The signature covers the tool name, its input, and the result. A tool
/// round that produces any signature not seen before in the turn (an edit, a
/// new command, a changed result) is progress and closes the window, so an
/// edit-then-verify cycle never trips even when the verify step (a passing
/// typecheck) returns identical output each time. `seen` outlives the window,
/// so a cycle that only replays old signatures (same call, or A/B alternating)
/// still accumulates counts and trips.
#[derive(Default)]
struct ToolLoopTracker {
    counts: std::collections::HashMap<u64, usize>,
    seen: std::collections::HashSet<u64>,
}

impl ToolLoopTracker {
    /// Records the tool calls and results appended by the most recent tool
    /// turn, returning true once any identical call has repeated enough times
    /// without progress in between.
    fn record(&mut self, messages: &[ChatMessage]) -> bool {
        let Some(calls) = messages.iter().rev().find_map(tool_call_inputs) else {
            return false;
        };
        let Some(results) = messages.iter().rev().find_map(tool_result_texts) else {
            return false;
        };

        let signatures: Vec<u64> = calls
            .iter()
            .filter_map(|(id, (name, input))| {
                results
                    .get(id)
                    .map(|result| tool_call_signature(name, input, result))
            })
            .collect();

        let mut progressed = false;
        for signature in &signatures {
            progressed |= self.seen.insert(*signature);
        }
        if progressed {
            self.counts.clear();
        }

        let mut tripped = false;
        for signature in signatures {
            let count = self.counts.entry(signature).or_insert(0);
            *count += 1;
            if *count >= MAX_IDENTICAL_TOOL_CALLS {
                tripped = true;
            }
        }
        tripped
    }

    /// Starts a fresh no-progress window, keeping the seen set.
    fn reset_counts(&mut self) {
        self.counts.clear();
    }
}

/// Extracts `tool_use_id -> (name, input)` from an assistant message.
fn tool_call_inputs(message: &ChatMessage) -> Option<Vec<(String, (String, String))>> {
    let MessageContent::Blocks(blocks) = &message.content else {
        return None;
    };
    let calls: Vec<(String, (String, String))> = blocks
        .iter()
        .filter_map(|block| match block {
            ChatContentBlock::ToolUse {
                id, name, input, ..
            } => Some((id.clone(), (name.clone(), input.to_string()))),
            _ => None,
        })
        .collect();
    (!calls.is_empty()).then_some(calls)
}

/// Extracts `tool_use_id -> result text` from a tool-result message.
fn tool_result_texts(message: &ChatMessage) -> Option<std::collections::HashMap<String, String>> {
    let MessageContent::Blocks(blocks) = &message.content else {
        return None;
    };
    let results: std::collections::HashMap<String, String> = blocks
        .iter()
        .filter_map(|block| match block {
            ChatContentBlock::ToolResult(result) => Some((
                result.tool_use_id.clone(),
                result.content.as_text().unwrap_or_default().to_string(),
            )),
            _ => None,
        })
        .collect();
    (!results.is_empty()).then_some(results)
}

fn tool_call_signature(name: &str, input: &str, result: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    input.hash(&mut hasher);
    result.hash(&mut hasher);
    hasher.finish()
}

/// Text that replaces an image a provider rejected as invalid, so the retry
/// sends no image bytes for the file the provider already refused.
fn omitted_invalid_image_text(path: Option<&str>) -> String {
    match path {
        Some(path) => format!("[image omitted: invalid image data at {path}]"),
        None => "[image omitted: invalid image data]".to_string(),
    }
}

/// The wire-message index a provider error names (`input[<n>]`), when it
/// carries one. Wire messages are a 1:1 projection of the live history, so
/// the index addresses the same message here.
fn invalid_image_input_index(err: &ProviderError) -> Option<usize> {
    let haystack = format!("{} {}", err.message, err.details.as_deref().unwrap_or(""));
    let start = haystack.find("input[")? + "input[".len();
    let rest = &haystack[start..];
    let end = rest.find(']')?;
    rest[..end].trim().parse().ok()
}

/// The file a tool-result image came from, from the JSON envelope in its
/// text block, when the result carries one.
fn tool_result_image_path(result: &ToolResult) -> Option<String> {
    let value: Value = serde_json::from_str(result.content.as_text()?).ok()?;
    let data = value.get("data").unwrap_or(&value);
    data.get("file_path")
        .or_else(|| data.get("resolved_file_path"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Replaces every image in one message with omitted-image text, returning
/// true when anything was replaced. Covers inline user images and images
/// carried inside tool results (the `Read` path).
fn strip_images_from_message(message: &mut ChatMessage) -> bool {
    let MessageContent::Blocks(blocks) = &mut message.content else {
        return false;
    };
    let mut stripped = false;
    for block in blocks.iter_mut() {
        match block {
            ChatContentBlock::Image { .. } => {
                *block = ChatContentBlock::text(omitted_invalid_image_text(None));
                stripped = true;
            }
            ChatContentBlock::ToolResult(result) => {
                let path = tool_result_image_path(result);
                let ToolResultContent::Blocks(inner) = &mut result.content else {
                    continue;
                };
                for inner_block in inner.iter_mut() {
                    if matches!(inner_block, ToolResultBlock::Image { .. }) {
                        *inner_block = ToolResultBlock::Text {
                            text: omitted_invalid_image_text(path.as_deref()),
                        };
                        stripped = true;
                    }
                }
            }
            _ => {}
        }
    }
    stripped
}

/// Strips the image a provider rejected as invalid from live history.
///
/// Targets the message the error names when it names one, and falls back to
/// every image in history when it does not (or when that message holds no
/// image). Returns true when an image was actually replaced.
fn strip_invalid_images(messages: &mut [ChatMessage], err: &ProviderError) -> bool {
    if let Some(index) = invalid_image_input_index(err)
        && let Some(message) = messages.get_mut(index)
        && strip_images_from_message(message)
    {
        return true;
    }
    messages.iter_mut().any(strip_images_from_message)
}

/// Tools whose successful result proves this turn started or reached work in
/// another thread.
const DISPATCH_TOOLS: &[&str] = &["create_thread", "send_thread_message", "invoke_subagent"];

/// Appended to a final answer that claims work started when this turn
/// dispatched nothing. It is persisted with the answer, so the next turn sees
/// it too.
const UNBACKED_DISPATCH_CORRECTION: &str = "Correction from ZDX: nothing was started. This turn \
     made no create_thread, send_thread_message, or invoke_subagent call, so no new work is \
     running for this request. Ask again to start it.";

/// True when the turn offers `create_thread`. Only such turns (the
/// orchestrator's) can start a worker, so only they get the unbacked-dispatch
/// check. Workers, helpers, and jobs report on work other threads did; their
/// text stays untouched.
fn offers_worker_dispatch(enabled_tools: &HashSet<String>) -> bool {
    enabled_tools
        .iter()
        .any(|name| name.eq_ignore_ascii_case("create_thread"))
}

/// True when a tool-result JSON envelope names the thread the call started or
/// reached: `thread_id` (`create_thread`), `worker.thread_id`
/// (`send_thread_message`), or `child_thread_id` (`invoke_subagent`), inside
/// or outside the envelope's `data` object.
fn json_carries_thread_id(text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let data = value.get("data").unwrap_or(&value);
    [data, &value]
        .into_iter()
        .flat_map(|object| {
            [
                object.get("thread_id"),
                object.get("child_thread_id"),
                object
                    .get("worker")
                    .and_then(|worker| worker.get("thread_id")),
            ]
        })
        .any(|id| id.and_then(Value::as_str).is_some_and(|id| !id.is_empty()))
}

/// True when `turn_messages` (this turn's messages only) hold a dispatch: a
/// [`DISPATCH_TOOLS`] call whose result returned a thread id.
///
/// Older dispatches in the thread do not count: they prove nothing about
/// what this turn started.
fn has_worker_dispatch(turn_messages: &[ChatMessage]) -> bool {
    let mut dispatch_ids: HashSet<String> = HashSet::new();
    for message in turn_messages {
        let MessageContent::Blocks(blocks) = &message.content else {
            continue;
        };
        for block in blocks {
            if let ChatContentBlock::ToolUse { id, name, .. } = block
                && DISPATCH_TOOLS
                    .iter()
                    .any(|tool| name.eq_ignore_ascii_case(tool))
            {
                dispatch_ids.insert(id.clone());
            }
        }
    }
    if dispatch_ids.is_empty() {
        return false;
    }
    turn_messages.iter().any(|message| {
        let MessageContent::Blocks(blocks) = &message.content else {
            return false;
        };
        blocks.iter().any(|block| {
            matches!(block, ChatContentBlock::ToolResult(result)
                if dispatch_ids.contains(&result.tool_use_id)
                    && !result.is_error
                    && result.content.as_text().is_some_and(json_carries_thread_id))
        })
    })
}

/// Phrases that make a sentence a negation, a condition, or an offer, not a
/// claim that work started. Matched on the lowercased sentence padded with
/// spaces.
const NOT_CLAIMS: &[&str] = &[
    "no worker",
    "no new worker",
    "not started",
    "didn't start",
    "did not start",
    "hasn't started",
    "haven't started",
    "never started",
    "nothing was started",
    "nothing started",
    "without starting",
    "failed to start",
    "unable to start",
    " if i ",
    " if a ",
    " if you ",
    " i can ",
    " can start ",
    " could start ",
    " should i ",
    " shall i ",
    " want me to ",
    " would you like ",
];

/// Verbs that open a sentence announcing work as under way ("Retrying the
/// fix now.", "Installing the new bundle on r11.").
const ACTION_OPENERS: &[&str] = &[
    "starting",
    "restarting",
    "retrying",
    "rerunning",
    "re-running",
    "making",
    "installing",
    "reinstalling",
    "building",
    "rebuilding",
    "fixing",
    "running",
    "launching",
    "relaunching",
    "spawning",
    "dispatching",
    "delegating",
    "kicking off",
    "creating",
    "applying",
    "deploying",
    "remoting",
    "porting",
    "pushing",
    "committing",
];

/// Words after an action verb that make the sentence a status report
/// ("Running for 41 min", "Building since 20:22"), not an announcement.
const STATUS_WORDS: &[&str] = &[
    "for", "since", "until", "at", "in", "as", "fine", "smoothly", "still", "slowly", "well",
];

/// Filler that can open a reply before the real first sentence ("On it —
/// starting …").
const FILLER_OPENERS: &[&str] = &["on it", "okay", "ok", "got it", "sure", "yes", "right"];

/// Verbs that, after a present-perfect first-person subject ("I've
/// started"), claim a dispatch this reply made. Plain past tense ("the worker
/// I started") is left alone: it usually refers to an earlier turn.
const DONE_VERBS: &[&str] = &[
    "started",
    "launched",
    "spawned",
    "kicked off",
    "restarted",
    "dispatched",
    "delegated",
    "queued",
    "created",
];

/// Lowercases a sentence and drops Markdown emphasis, heading, quote, and
/// list markers around it.
fn normalize_sentence(sentence: &str) -> String {
    sentence
        .trim()
        .trim_start_matches(|c: char| {
            matches!(c, '*' | '_' | '#' | '>' | '-' | '`') || c.is_whitespace()
        })
        .trim_end_matches(|c: char| matches!(c, '*' | '_' | '`') || c.is_whitespace())
        .to_lowercase()
}

/// Removes a filler opener ("on it —", "ok,") from a normalized sentence.
fn strip_filler(sentence: &str) -> &str {
    for filler in FILLER_OPENERS {
        if let Some(rest) = sentence.strip_prefix(filler)
            && rest.chars().next().is_none_or(|c| !c.is_alphanumeric())
        {
            return rest.trim_start_matches(|c: char| {
                matches!(c, '—' | '–' | '-' | ',' | ':' | '.' | '!' | '*') || c.is_whitespace()
            });
        }
    }
    sentence
}

/// Splits text into sentences at `.`, `!`, or `?` followed by whitespace or
/// the end, and at line breaks.
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut start = 0;
        let mut chars = line.char_indices().peekable();
        while let Some((index, ch)) = chars.next() {
            let at_boundary = chars.peek().is_none_or(|(_, next)| next.is_whitespace());
            if matches!(ch, '.' | '!' | '?') && at_boundary {
                let end = index + ch.len_utf8();
                out.push(&line[start..end]);
                start = end;
            }
        }
        out.push(&line[start..]);
    }
    out.into_iter()
        .filter(|sentence| !sentence.trim().is_empty())
        .collect()
}

/// True when one normalized sentence claims work was started in this reply.
///
/// `opening` marks the reply's first real sentence, where an action verb
/// alone ("Installing the new bundle on r11.") is a claim; elsewhere the
/// sentence must also say "now".
fn is_dispatch_claim(sentence: &str, opening: bool) -> bool {
    let padded = format!(" {sentence} ");
    if sentence.ends_with('?') || NOT_CLAIMS.iter().any(|phrase| padded.contains(phrase)) {
        return false;
    }
    let says_now = ["for now", "by now", "until now", "as of now"]
        .iter()
        .fold(padded.clone(), |text, phrase| text.replace(phrase, " "))
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word == "now");
    let action = ["i'm ", "i am ", "we're ", "we are "]
        .iter()
        .find_map(|subject| sentence.strip_prefix(subject))
        .unwrap_or(sentence);
    let opens_with_action = ACTION_OPENERS.iter().any(|verb| {
        action
            .strip_prefix(verb)
            .and_then(|rest| rest.strip_prefix(' '))
            .and_then(|rest| rest.split_whitespace().next())
            .is_some_and(|next| {
                next.starts_with(|c: char| c.is_alphanumeric() || c == '`')
                    && !STATUS_WORDS.contains(&next)
            })
    });
    if opens_with_action && (opening || says_now) {
        return true;
    }
    ["i've", "i have", "we've", "we have"]
        .iter()
        .flat_map(|subject| {
            DONE_VERBS
                .iter()
                .map(move |verb| format!(" {subject} {verb} "))
        })
        .any(|phrase| padded.contains(&phrase))
}

/// True when `text` claims that work was started in this reply.
fn has_dispatch_claim(text: &str) -> bool {
    let mut opening = true;
    for sentence in sentences(text) {
        let normalized = normalize_sentence(sentence);
        let body = strip_filler(&normalized);
        if body.is_empty() {
            continue;
        }
        if is_dispatch_claim(body, opening) {
            return true;
        }
        opening = false;
    }
    false
}

/// Closes a turn early without an error: the loop guard stopped the model, and
/// the user gets a notice plus a thread they can continue from.
fn finish_turn_early(
    messages: Vec<ChatMessage>,
    sender: &EventSender,
    prior_message_count: usize,
) -> (String, Vec<ChatMessage>) {
    let final_text = String::new();
    sender.send(AgentEvent::TurnFinished {
        status: TurnStatus::Completed,
        final_text: final_text.clone(),
        messages: messages.clone(),
        prior_message_count,
    });
    (final_text, messages)
}

/// Rebuilds the run setup one thinking level down, for the single retry the
/// loop guard attempts. Returns `None` when there is nothing left to lower or
/// the rebuild fails; the caller then closes the turn with a notice.
fn rebuild_for_lower_thinking(
    setup: &RunTurnSetup,
    config: &Config,
    options: &AgentOptions,
    thread_id: Option<&str>,
    messages: &[ChatMessage],
) -> Option<RunTurnSetup> {
    let lower = lowered_thinking_level(setup.thinking_level)?;
    match build_run_turn_setup(config, options, thread_id, messages, Some(lower)) {
        Ok(mut rebuilt) => {
            rebuilt
                .tool_ctx
                .turn_tasks
                .clone_from(&setup.tool_ctx.turn_tasks);
            Some(rebuilt)
        }
        Err(err) => {
            tracing::warn!(error = %err, "Failed to rebuild setup for lower thinking level");
            None
        }
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

/// Reasoning budget for a single assistant turn that has produced no tool call
/// and no visible text. A model stuck emitting filler ("Let me run." repeated
/// until the output limit) otherwise burns the whole `max_tokens` budget over
/// many minutes and, once persisted, inflates every later request. Set well
/// above any legitimate thinking block — the largest observed in real threads
/// is ~26k characters — so it only fires on a degenerate turn.
const RUNAWAY_REASONING_CHARS: usize = 160_000;
/// Reasoning kept per block when the guard trips: enough to show what the
/// model was doing, small enough to keep the thread file usable.
const RUNAWAY_REASONING_KEEP_CHARS: usize = 4_000;
/// Answer text kept when the guard trips on the visible answer.
const RUNAWAY_TEXT_KEEP_CHARS: usize = 4_000;
/// How many times one identical tool call (same tool, same input, same result)
/// may repeat within a turn before the turn is stopped.
const MAX_IDENTICAL_TOOL_CALLS: usize = 3;
/// Reasoning kept per block when persisting. Blocks above this are truncated
/// head-and-tail so a degenerate turn cannot make the thread unloadable.
const MAX_PERSISTED_REASONING_CHARS: usize = 128_000;

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
            if matches!(err, TurnError::Interrupted { .. }) {
                tracing::debug!(thread = thread_id.unwrap_or("-"), "Turn interrupted");
            } else {
                tracing::warn!(
                    thread = thread_id.unwrap_or("-"),
                    error = %err.log_summary(),
                    "Turn failed"
                );
            }
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
#[tracing::instrument(
    name = "turn",
    skip_all,
    fields(
        thread = thread_id.unwrap_or("-"),
        model = tracing::field::Empty,
        provider = tracing::field::Empty,
    )
)]
async fn run_turn_inner(
    messages: Vec<ChatMessage>,
    config: &Config,
    options: &AgentOptions,
    system_prompt: Option<&str>,
    thread_id: Option<&str>,
    sender: &EventSender,
    cancel: Option<&CancellationToken>,
) -> RunTurnResult {
    // Every exit closes the steer source; the explicit closes below run before
    // the terminal event, so an owner reacting to it sees the source closed.
    let _steer_close = crate::core::steer::CloseOnDrop(options.steer.as_ref());
    // A persistent-profile thread has one writer: the engine service.
    // Any other run on it would interleave with the service's turns.
    if let Some(thread_id) = thread_id
        && options.activity_kind.as_deref() != Some(crate::orchestration::ACTIVITY_KIND)
        && let Err(err) = crate::orchestration::ensure_direct_run_allowed(thread_id)
    {
        return Err((TurnError::from_anyhow(err), messages));
    }
    let _run_lease = match thread_id {
        Some(thread_id) => match options
            .run_lease
            .as_ref()
            .filter(|lease| lease.thread_id() == thread_id)
        {
            Some(lease) => Some(Arc::clone(lease)),
            None => match crate::core::thread_lease::ThreadRunLease::try_acquire(thread_id) {
                Ok(lease) => Some(Arc::new(lease)),
                Err(err) => return Err((TurnError::from_anyhow(err), messages)),
            },
        },
        None => None,
    };
    let mut setup = build_run_turn_setup(config, options, thread_id, &messages, None)
        .map_err(|e| (TurnError::from_anyhow(e), messages.clone()))?;
    // Background work started by this turn's tool calls is stopped when the
    // guard drops, however the turn ends.
    let (_turn_tasks_guard, turn_tasks) = crate::tools::subagent_job::TurnTasksGuard::new();
    setup.tool_ctx.turn_tasks = Some(turn_tasks);
    let span = tracing::Span::current();
    span.record("model", setup.model.as_str());
    span.record("provider", setup.provider.as_str());
    tracing::info!(
        thinking = setup.thinking_level.display_name(),
        tools = setup.tools.len(),
        kind = options.activity_kind.as_deref().unwrap_or("main"),
        subagent = options.activity_subagent_name.as_deref().unwrap_or("-"),
        "Turn started"
    );
    let run_guard = crate::agent_activity::start(crate::agent_activity::StartParams {
        thread_id,
        surface: options.surface.as_deref(),
        model: Some(setup.model.as_str()),
        provider: Some(setup.provider.as_str()),
        account: setup.account.as_deref(),
        thinking: Some(setup.thinking_level.display_name()),
        kind: options.activity_kind.as_deref(),
        parent_thread_id: options.activity_parent_thread_id.as_deref(),
        subagent_name: options.activity_subagent_name.as_deref(),
    })
    .map(Arc::new);
    // Stops on its own once the guard drops at the end of the turn.
    if let Some(guard) = &run_guard {
        crate::agent_activity::spawn_heartbeat(guard);
    }
    // Every event this turn emits flows through the activity-bound sender so
    // the run marker tracks phase transitions without per-site hooks.
    let sender = &sender.clone().with_activity(run_guard.clone());
    let mut messages = messages;
    let initial_message_count = messages.len();
    let mut consecutive_malformed_tool_turns = 0usize;
    // The output-loop guard retries once at a lower thinking level before it
    // closes the turn with a notice.
    let mut thinking_retry_used = false;
    let mut tool_loop_tracker = ToolLoopTracker::default();
    // One iteration of the outer loop = one model completion (plus any tool
    // round it requests). Tracked only for logging; nothing else needs it.
    let mut model_turn: u32 = 0;
    let model_reads_images =
        crate::models::model_reads_images(&config.providers, &setup.provider, &setup.model);
    let stream_idle_timeout = config.stream_idle_timeout();

    loop {
        ensure_not_interrupted(None, cancel).map_err(|e| (e, messages.clone()))?;
        model_turn += 1;

        // Runs every model turn so images arriving mid-turn (e.g. a `Read` on a
        // screenshot) are swapped too, not just the ones already in history.
        if !model_reads_images {
            let replaced =
                crate::core::media_fallback::replace_images_with_ask_media_notes(&mut messages);
            if replaced > 0 {
                tracing::debug!(replaced, "Replaced images with ask-media notes");
            }
        }

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
            let mut invalid_image_retry_used = false;
            'retry: loop {
                ensure_not_interrupted(None, cancel).map_err(|e| (e, messages.clone()))?;

                let outcome: std::result::Result<
                    StreamState,
                    (TurnError, bool, Option<StreamState>),
                > = {
                    let request_start = RequestSpanStart {
                        instant: Instant::now(),
                        wall: crate::core::thread_persistence::chrono_timestamp(),
                    };
                    tracing::debug!(model_turn, attempt, "Provider request attempt");
                    // Project each user message's persisted runtime-context
                    // block onto its wire text. Deterministic and idempotent,
                    // so transparent retries reuse identical bytes; the
                    // original `messages` (pure text + context) still feed
                    // persistence and interrupted-turn snapshots.
                    let wire_messages: Vec<ChatMessage> = messages
                        .iter()
                        .map(ChatMessage::with_runtime_context_projected)
                        .collect();
                    match request_stream(
                        &setup.client,
                        &wire_messages,
                        &setup.tools,
                        system_prompt,
                        cancel,
                        stream_idle_timeout,
                    )
                    .await
                    {
                        Ok(stream) => {
                            match consume_stream_with_start(
                                stream,
                                &messages,
                                sender,
                                cancel,
                                &setup.model,
                                &setup.provider,
                                request_start,
                                stream_idle_timeout,
                            )
                            .await
                            {
                                Ok(state) => Ok(state),
                                Err((err, state)) => {
                                    let can_retry = can_transparently_retry_stream(&state);
                                    Err((err, can_retry, Some(state)))
                                }
                            }
                        }
                        Err(err) => Err((err, true, None)),
                    }
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
                        // A provider 400 "invalid image" is not transient:
                        // retrying the same bytes would fail again. Strip the
                        // offending image from live history and retry once.
                        // Only when nothing user-visible has been emitted
                        // (`state` is none), so the retry cannot rewind
                        // partial output. Defense in depth: `Read` already
                        // fails closed on undecodable images.
                        if !invalid_image_retry_used
                            && state.is_none()
                            && let TurnError::Provider(provider_err) = &err
                            && provider_err.is_invalid_image()
                            && strip_invalid_images(&mut messages, provider_err)
                        {
                            invalid_image_retry_used = true;
                            attempt += 1;
                            tracing::warn!(
                                attempt,
                                error = %provider_err.message,
                                "Provider rejected an image; stripped it and retrying once"
                            );
                            sender.send(AgentEvent::ProviderRetry {
                                kind: ErrorKind::from(provider_err.kind.clone()),
                                message: provider_err.message.clone(),
                                details: provider_err.details.clone(),
                                attempt,
                                max_retries: MAX_RETRIES,
                                delay_ms: 0,
                            });
                            continue 'retry;
                        }
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
        if let Some(loop_kind) = stream_state.output_loop {
            if !thinking_retry_used
                && let Some(rebuilt) =
                    rebuild_for_lower_thinking(&setup, config, options, thread_id, &messages)
            {
                thinking_retry_used = true;
                tracing::warn!(
                    model_turn,
                    reason = ?loop_kind,
                    thinking = rebuilt.thinking_level.display_name(),
                    "Output loop detected; retrying once at a lower thinking level"
                );
                sender.send(AgentEvent::Notice {
                    kind: NoticeKind::OutputLoop,
                    message: format!(
                        "{} Retrying once at thinking level \"{}\".",
                        loop_kind.message(),
                        rebuilt.thinking_level.display_name()
                    ),
                    details: Some(loop_kind.details().to_string()),
                });
                setup = rebuilt;
                continue;
            }
            sender.send(AgentEvent::Notice {
                kind: NoticeKind::OutputLoop,
                message: loop_kind.message().to_string(),
                details: Some(loop_kind.details().to_string()),
            });
        }

        if stream_state.needs_tool_execution() {
            let stats = process_tool_turn(
                &mut messages,
                &mut stream_state.turn,
                &setup,
                sender,
                cancel,
                initial_message_count,
                run_guard.as_deref(),
                options.steer.as_ref(),
            )
            .await
            .map_err(|e| (e, messages.clone()))?;
            // New input is progress: the loop guards must not end a turn that
            // just took a steer the model has not seen yet.
            if stats.steered > 0 {
                consecutive_malformed_tool_turns = 0;
                tool_loop_tracker.reset_counts();
                continue;
            }
            if stats.executable == 0 && stats.malformed > 0 {
                consecutive_malformed_tool_turns += 1;
                if consecutive_malformed_tool_turns >= MAX_CONSECUTIVE_MALFORMED_TOOL_TURNS {
                    tracing::warn!(
                        model_turn,
                        malformed = stats.malformed,
                        consecutive = consecutive_malformed_tool_turns,
                        "Aborting turn: provider kept requesting tools with invalid arguments"
                    );
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
            if tool_loop_tracker.record(&messages) {
                if !thinking_retry_used
                    && let Some(rebuilt) =
                        rebuild_for_lower_thinking(&setup, config, options, thread_id, &messages)
                {
                    thinking_retry_used = true;
                    tool_loop_tracker.reset_counts();
                    tracing::warn!(
                        model_turn,
                        thinking = rebuilt.thinking_level.display_name(),
                        "Identical tool call repeated; retrying once at a lower thinking level"
                    );
                    sender.send(AgentEvent::Notice {
                        kind: NoticeKind::OutputLoop,
                        message: format!(
                            "{} Retrying once at thinking level \"{}\".",
                            OutputLoop::ToolRepetition.message(),
                            rebuilt.thinking_level.display_name()
                        ),
                        details: Some(OutputLoop::ToolRepetition.details().to_string()),
                    });
                    setup = rebuilt;
                    continue;
                }
                tracing::warn!(
                    model_turn,
                    calls = MAX_IDENTICAL_TOOL_CALLS,
                    "Aborting turn: identical tool call repeated with an unchanged result"
                );
                sender.send(AgentEvent::Notice {
                    kind: NoticeKind::OutputLoop,
                    message: OutputLoop::ToolRepetition.message().to_string(),
                    details: Some(OutputLoop::ToolRepetition.details().to_string()),
                });
                close_steer(options);
                return Ok(finish_turn_early(messages, sender, initial_message_count));
            }
            continue;
        }

        tracing::info!(model_turns = model_turn, "Turn finished");
        // A steer that arrives during the final answer stays unread: the
        // answer is delivered unchanged and the owner runs the steer next.
        close_steer(options);
        return Ok(finalize_non_tool_turn(
            &mut messages,
            stream_state.turn,
            sender,
            initial_message_count,
            offers_worker_dispatch(&setup.enabled_tools),
        ));
    }
}

struct RunTurnSetup {
    model: String,
    /// Provider id serving the request (e.g. `anthropic`, `claude-cli`, or a
    /// custom provider name). Attached to emitted usage events for attribution,
    /// so it stays account-free — pricing and usage aggregate per provider.
    provider: String,
    /// Named OAuth account serving the request, when the model targets a
    /// non-default account (`provider@account:model`). Kept out of `provider`
    /// so usage attribution and pricing lookups stay keyed by provider kind.
    account: Option<String>,
    /// Thinking/reasoning level effectively used for this run.
    thinking_level: ThinkingLevel,
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
    messages: &[ChatMessage],
    thinking_override: Option<ThinkingLevel>,
) -> Result<RunTurnSetup> {
    let spec = crate::models::ModelSpec::parse(&config.model);
    if let Some((custom_cfg, bare_model)) = config.providers.custom_provider_for_model(spec.base) {
        let provider_name = spec
            .base
            .split_once(':')
            .or_else(|| spec.base.split_once('/'))
            .map(|(prefix, _)| prefix.trim().to_string())
            .unwrap_or_default();
        return build_custom_run_turn_setup(
            config,
            options,
            thread_id,
            custom_cfg,
            bare_model,
            provider_name,
            messages,
            thinking_override,
        );
    }

    let selection = resolve_provider(&config.model);
    let provider = selection.kind;
    let max_tokens = config.effective_max_tokens_for_request(
        spec.base,
        Some(zdx_types::estimated_input_tokens(messages)),
    );
    let thinking_level = crate::models::effective_thinking_level(
        spec.base,
        thinking_override.unwrap_or(config.thinking_level),
    );
    let provider_config = config.providers.get(provider);
    let provider_ctx = ProviderBuildContext {
        model: &selection.model,
        provider,
        max_tokens,
        config_max_tokens: config.max_tokens,
        thinking_level,
        cache_key: thread_id
            .or_else(|| {
                (provider == ProviderKind::OpencodeGo)
                    .then_some(options.conversation_id.as_deref())
                    .flatten()
            })
            .map(str::to_owned),
        text_verbosity: options.text_verbosity,
        service_tier: options
            .service_tier
            .as_deref()
            .or(if selection.fast && provider.supports_priority_tier() {
                Some("priority")
            } else {
                None
            })
            .map(str::to_owned),
        base_url: provider_config.effective_base_url(),
        api_key: provider_config.effective_api_key(),
        provider_text_verbosity: provider_config.effective_text_verbosity(),
        websocket: provider_config.websocket,
        api_hint: if matches!(
            provider,
            ProviderKind::OpencodeGo | ProviderKind::GithubCopilot
        ) {
            crate::models::ModelOption::find_by_provider_and_id(provider.id(), &selection.model)
                .and_then(|m| m.capabilities.api)
                .map(ToString::to_string)
        } else {
            None
        },
        account: selection.account.as_deref(),
    };
    let client = provider.build_client(&provider_ctx)?;
    let tool_ctx = turn_tool_context(
        config,
        options,
        thread_id,
        &crate::config::paths::zdx_home(),
    );
    let tool_registry = options.tool_config.registry.clone();
    let tools = resolve_tools(
        config,
        options,
        provider_config,
        matches!(provider, ProviderKind::OpenAI | ProviderKind::OpenAICodex),
        &tool_registry,
    );
    let enabled_tools = tools.iter().map(|t| t.name.clone()).collect();

    Ok(RunTurnSetup {
        model: selection.model,
        provider: provider.id().to_string(),
        account: selection.account,
        thinking_level,
        client,
        tools,
        enabled_tools,
        tool_ctx,
        tool_registry,
    })
}

/// Builds the tool context one turn's tool calls run with. The thread's
/// runtime environment (`ZDX_HOME`, `ZDX_ARTIFACT_DIR`, `ZDX_THREAD_ID`,
/// `ZDX_MEMORY_ROOT`) is carried here, per turn, so concurrent turns in one
/// process never read each other's values from the process environment.
fn turn_tool_context(
    config: &Config,
    options: &AgentOptions,
    thread_id: Option<&str>,
    zdx_home: &std::path::Path,
) -> ToolContext {
    ToolContext::new(
        options
            .root
            .canonicalize()
            .unwrap_or_else(|_| options.root.clone()),
        options.tool_timeout,
    )
    .with_current_thread_id(thread_id)
    .with_config(config)
    .with_allowed_subagents(options.tool_config.allowed_subagents.clone())
    .with_skill_scope(options.tool_config.skill_scope.clone())
    .with_background_execution(background_execution(config, options))
    .with_env(crate::core::context::runtime_tool_env_with_zdx_home(
        config, thread_id, zdx_home,
    ))
}

/// Builds the run-turn setup for a custom provider
/// (`[providers.custom.<name>]`): no `ProviderKind`, default tool set. The
/// wire protocol comes from an explicit per-model `api` in
/// `model_overrides.toml` when present, else the provider's `api`.
#[allow(clippy::too_many_arguments)]
fn build_custom_run_turn_setup(
    config: &Config,
    options: &AgentOptions,
    thread_id: Option<&str>,
    custom_cfg: &crate::config::CustomProviderConfig,
    bare_model: String,
    provider_name: String,
    messages: &[ChatMessage],
    thinking_override: Option<ThinkingLevel>,
) -> Result<RunTurnSetup> {
    let thinking_enabled = crate::models::model_supports_reasoning(&config.model)
        && thinking_override
            .unwrap_or(config.thinking_level)
            .is_enabled();
    let thinking_level = if thinking_enabled {
        thinking_override.unwrap_or(config.thinking_level)
    } else {
        ThinkingLevel::Off
    };
    let api = match crate::models::user_model_api_override(&provider_name, &bare_model) {
        Some(id) => CustomProviderApi::from_id(&id).ok_or_else(|| {
            anyhow::anyhow!(
                "model override `{provider_name}:{bare_model}` has unsupported api `{id}` \
                 (expected `chat-completions`/`openai-completions` or `anthropic`/`anthropic-messages`)"
            )
        })?,
        None => custom_cfg.api,
    };
    let base_url = custom_cfg.effective_base_url(api)?;
    let api_key = custom_cfg.resolve_api_key()?;
    let client = match api {
        CustomProviderApi::ChatCompletions => crate::providers::openai_compatible::build_custom(
            base_url,
            api_key,
            bare_model.clone(),
            config.max_tokens,
            thread_id.map(str::to_owned),
            thinking_level,
        ),
        CustomProviderApi::Anthropic => anthropic::api::build_custom(
            base_url,
            api_key,
            bare_model.clone(),
            config.effective_max_tokens_for_request(
                crate::models::ModelSpec::parse(&config.model).base,
                Some(zdx_types::estimated_input_tokens(messages)),
            ),
            thinking_level,
        ),
    };

    let tool_ctx = turn_tool_context(
        config,
        options,
        thread_id,
        &crate::config::paths::zdx_home(),
    );
    let tool_registry = options.tool_config.registry.clone();
    let provider_config = crate::config::ProviderConfig::default();
    let tools = resolve_tools(config, options, &provider_config, false, &tool_registry);
    let enabled_tools = tools.iter().map(|t| t.name.clone()).collect();

    Ok(RunTurnSetup {
        model: bare_model,
        provider: provider_name,
        account: None,
        thinking_level,
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
    let provider_config = config.providers.get(provider);
    let use_codex_toolset = matches!(provider, ProviderKind::OpenAI | ProviderKind::OpenAICodex);
    resolve_tools(
        config,
        options,
        provider_config,
        use_codex_toolset,
        &options.tool_config.registry,
    )
}

fn resolve_tools(
    config: &Config,
    options: &AgentOptions,
    provider_config: &crate::config::ProviderConfig,
    use_codex_toolset: bool,
    tool_registry: &ToolRegistry,
) -> Vec<ToolDefinition> {
    let mut tools = match &options.tool_config.selection {
        ToolSelection::Auto { base, include } => {
            let base_tools = if provider_config.tools.is_some() {
                tool_registry.tools_for_provider(provider_config)
            } else {
                let tool_set = if use_codex_toolset {
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
                        *tool = crate::tools::subagent::definition_with_subagents(
                            &available_subagents,
                            options.tool_config.allowed_subagents.as_deref(),
                        );
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    "Failed to discover subagents for Invoke_Subagent tool description"
                );
            }
        }
    } else {
        tools.retain(|tool| !tool.name.eq_ignore_ascii_case("Invoke_Subagent"));
    }

    apply_background_execution(
        &mut tools,
        background_execution(config, options),
        tool_registry,
    );
    tools
}

/// Resolves the run's background-execution policy from its explicit
/// permission, its surface, config, and its tool deadline. The single source
/// for the tool list, the `Bash` and `invoke_subagent` schemas, and runtime
/// enforcement.
fn background_execution(config: &Config, options: &AgentOptions) -> BackgroundExecution {
    BackgroundExecution::resolve(
        options.tool_config.background_commands,
        options.surface.as_deref(),
        config,
    )
}

/// Makes the tool list match the run's background-execution policy.
///
/// With background execution, a run that has a tool that can start
/// background work (`bash`, `invoke_subagent`) also gets the tools that read
/// and stop jobs, so no job it starts is out of its reach, and
/// `invoke_subagent` advertises `background`. Without it, the management
/// tools are removed, `Bash` is described as foreground-only with no
/// implicit limit and no `background` property, and `invoke_subagent` keeps
/// its blocking schema.
fn apply_background_execution(
    tools: &mut Vec<ToolDefinition>,
    policy: BackgroundExecution,
    tool_registry: &ToolRegistry,
) {
    let named = |name: &str, list: &[&str]| list.iter().any(|tool| name.eq_ignore_ascii_case(tool));
    match policy {
        BackgroundExecution::Enabled { handoff_bound } => {
            let can_start = tools
                .iter()
                .any(|tool| named(&tool.name, &BACKGROUND_STARTING_TOOLS));
            if !can_start {
                return;
            }
            if cfg!(unix) {
                for tool in tools.iter_mut() {
                    if tool.name.eq_ignore_ascii_case("invoke_subagent") {
                        crate::tools::subagent::add_background_option(tool, handoff_bound);
                    }
                }
            }
            let missing: Vec<&str> = BACKGROUND_MANAGEMENT_TOOLS
                .into_iter()
                .filter(|name| {
                    !tools
                        .iter()
                        .any(|tool| tool.name.eq_ignore_ascii_case(name))
                })
                .collect();
            tools.extend(tool_registry.tools_from_names(missing));
        }
        BackgroundExecution::Disabled => {
            tools.retain(|tool| !named(&tool.name, &BACKGROUND_MANAGEMENT_TOOLS));
            for tool in tools.iter_mut() {
                if tool.name.eq_ignore_ascii_case("bash") {
                    *tool = crate::tools::bash::definition_without_background();
                }
            }
        }
    }
}

struct RequestSpanStart {
    instant: Instant,
    wall: String,
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
    /// Provider id serving this stream, attached to emitted `UsageUpdate`
    /// events for per-provider attribution. Empty in unit tests that build a
    /// `StreamState` directly and don't assert provider.
    provider: String,
    /// When this attempt's provider request was initiated (before the stream
    /// opened). Used to derive per-request latency on the terminal usage
    /// event. Defaults to construction time; `consume_stream` overrides it
    /// with the pre-request timestamp.
    request_started_at: Instant,
    /// Wall-clock counterpart to `request_started_at`, persisted so clients
    /// can place requests against concurrently executing spans.
    request_started_at_wall: String,
    /// When the first content token (text/reasoning/tool) arrived, for
    /// time-to-first-token. `None` if no content arrived this attempt.
    first_token_at: Option<Instant>,
    /// Set when the output-loop guard aborted this stream: the model was
    /// repeating itself instead of finishing the turn. The turn still
    /// completes (after at most one retry at a lower thinking level); the
    /// caller surfaces a notice and the repeated content is truncated before
    /// it reaches the thread file.
    output_loop: Option<OutputLoop>,
}

/// Why the output-loop guard stopped a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputLoop {
    /// Reasoning passed the per-turn character budget without producing text
    /// or a tool call.
    ReasoningBudget,
    /// Reasoning degenerated into the same few short lines.
    ReasoningRepetition,
    /// The visible answer degenerated into the same few short lines.
    TextRepetition,
    /// The turn kept re-issuing an identical tool call and getting an
    /// identical result back.
    ToolRepetition,
}

impl OutputLoop {
    fn message(self) -> &'static str {
        match self {
            Self::ReasoningBudget => {
                "Generation stopped: the model spent its whole reasoning budget without answering or calling a tool."
            }
            Self::ReasoningRepetition => {
                "Generation stopped: the model was repeating itself in reasoning without answering or calling a tool."
            }
            Self::TextRepetition => {
                "Generation stopped: the model was repeating itself in the answer."
            }
            Self::ToolRepetition => {
                "Generation stopped: the model kept repeating the same tool call with the same result."
            }
        }
    }

    fn details(self) -> &'static str {
        match self {
            Self::ReasoningBudget => "output_loop=reasoning_budget",
            Self::ReasoningRepetition => "output_loop=reasoning_repetition",
            Self::TextRepetition => "output_loop=text_repetition",
            Self::ToolRepetition => "output_loop=tool_repetition",
        }
    }
}

/// One step down the thinking ladder, for the single retry attempted after the
/// output-loop guard trips. `None` when there is nothing left to lower.
fn lowered_thinking_level(level: ThinkingLevel) -> Option<ThinkingLevel> {
    match level {
        ThinkingLevel::Max => Some(ThinkingLevel::XHigh),
        ThinkingLevel::XHigh => Some(ThinkingLevel::High),
        ThinkingLevel::High => Some(ThinkingLevel::Medium),
        ThinkingLevel::Medium => Some(ThinkingLevel::Low),
        ThinkingLevel::Low | ThinkingLevel::Off => None,
    }
}

impl StreamState {
    fn new(model: String) -> Self {
        Self {
            turn: AssistantTurnBuilder::new(model),
            stop_reason: None,
            usage_seen: crate::providers::Usage::default(),
            pending_usage: crate::providers::Usage::default(),
            emitted_visible_content: false,
            provider: String::new(),
            request_started_at: Instant::now(),
            request_started_at_wall: crate::core::thread_persistence::chrono_timestamp(),
            first_token_at: None,
            output_loop: None,
        }
    }

    fn needs_tool_execution(&self) -> bool {
        self.stop_reason.as_deref() == Some("tool_use") && self.turn.has_tool_uses()
    }

    /// Marks that visible assistant content was emitted, capturing the
    /// first-token timestamp on the initial transition.
    fn mark_visible_content(&mut self) {
        if self.first_token_at.is_none() {
            self.first_token_at = Some(Instant::now());
        }
        self.emitted_visible_content = true;
    }

    /// Whether the turn has committed to something the caller asked for:
    /// visible text or a tool call. Reasoning alone does not count, so a model
    /// that only thinks has not answered anything yet.
    fn has_committed_output(&self) -> bool {
        self.turn.parts.iter().any(|part| match part {
            AssistantPart::Text(text) => !text.text.is_empty(),
            AssistantPart::ToolUse(_) => true,
            AssistantPart::Reasoning(_) => false,
        })
    }

    /// Runs the reasoning half of the output-loop guard, returning the reason
    /// it tripped.
    ///
    /// Both checks are deliberately blind to a turn that has produced text or
    /// a tool call: a long deliberation that ends in an answer is not a
    /// runaway.
    fn check_output_loop(&self) -> Option<OutputLoop> {
        if self.has_committed_output() {
            return None;
        }
        let mut total = 0usize;
        let mut tail: Option<&str> = None;
        for part in &self.turn.parts {
            if let AssistantPart::Reasoning(reasoning) = part {
                total += reasoning.text.len();
                tail = Some(&reasoning.text);
            }
        }
        if total > RUNAWAY_REASONING_CHARS {
            return Some(OutputLoop::ReasoningBudget);
        }
        reasoning_repeats(tail?).then_some(OutputLoop::ReasoningRepetition)
    }

    /// Shrinks whatever the guard caught to a readable head and tail.
    ///
    /// A truncated reasoning block also loses its replay token: the signature
    /// no longer matches the surviving text, and a mismatch is worse than no
    /// replay at all.
    fn truncate_looped_output(&mut self) {
        for part in &mut self.turn.parts {
            match part {
                AssistantPart::Reasoning(reasoning) => {
                    if reasoning.text.len() > RUNAWAY_REASONING_KEEP_CHARS {
                        reasoning.text =
                            truncate_middle(&reasoning.text, RUNAWAY_REASONING_KEEP_CHARS);
                    }
                    reasoning.replay = None;
                    reasoning.signature.clear();
                    reasoning.signature_provider = None;
                }
                AssistantPart::Text(text) => {
                    if text.text.len() > RUNAWAY_TEXT_KEEP_CHARS {
                        text.text = truncate_middle(&text.text, RUNAWAY_TEXT_KEEP_CHARS);
                    }
                }
                AssistantPart::ToolUse(_) => {}
            }
        }
    }

    /// Emits one combined `AgentEvent::UsageUpdate` for any buffered usage
    /// since the last flush, then resets the buffer. No-op when the buffer
    /// is empty. Downstream consumers (TUI status bar, persistence, CLI
    /// exec) are additive, so a single combined event is equivalent to
    /// emitting each accumulated delta separately.
    fn flush_pending_usage(&mut self, sender: &EventSender) {
        self.emit_pending_usage(sender, None, None, None, None);
    }

    /// Terminal flush for a successful request: attaches per-request latency
    /// (wall-clock duration + time-to-first-token) to the emitted usage
    /// event. Used only on the `consume_stream` EOF-success path so timing
    /// is emitted exactly once per request. `UsagePersistor` merges a
    /// timing-only terminal update with any earlier token-bearing fragment.
    fn flush_final_usage(&mut self, sender: &EventSender) {
        let now = Instant::now();
        let duration_ms = u64::try_from(
            now.saturating_duration_since(self.request_started_at)
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        let ttft_ms = self.first_token_at.map(|t| {
            u64::try_from(
                t.saturating_duration_since(self.request_started_at)
                    .as_millis(),
            )
            .unwrap_or(u64::MAX)
        });
        self.emit_pending_usage(
            sender,
            Some(duration_ms),
            ttft_ms,
            Some(self.request_started_at_wall.clone()),
            Some(crate::core::thread_persistence::chrono_timestamp()),
        );
    }

    fn emit_pending_usage(
        &mut self,
        sender: &EventSender,
        duration_ms: Option<u64>,
        ttft_ms: Option<u64>,
        started_at: Option<String>,
        completed_at: Option<String>,
    ) {
        if self.pending_usage.is_empty() && duration_ms.is_none() {
            return;
        }
        let usage = std::mem::take(&mut self.pending_usage);
        sender.send(AgentEvent::UsageUpdate {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            model: self.turn.model.clone(),
            provider: self.provider.clone(),
            duration_ms,
            ttft_ms,
            started_at,
            completed_at,
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

#[tracing::instrument(name = "provider_request", skip_all)]
async fn request_stream(
    client: &dyn StreamingProvider,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    system_prompt: Option<&str>,
    cancel: Option<&CancellationToken>,
    idle_timeout: Option<Duration>,
) -> TurnResult<ProviderStream> {
    let stream_result = tokio::select! {
        biased;
        () = interrupt::wait_for_interrupt() => {
            return Err(TurnError::interrupted(None));
        }
        () = wait_for_cancel(cancel) => {
            return Err(TurnError::interrupted(None));
        }
        () = sleep_or_pending(idle_timeout) => {
            let limit = idle_timeout.unwrap_or_default();
            tracing::warn!(idle_secs = limit.as_secs(), "Provider request stalled before the stream opened");
            return Err(TurnError::Provider(stream_idle_error(limit)));
        }
        result = client.stream_messages(messages, tools, system_prompt) => result,
    };
    match stream_result {
        Ok(stream) => Ok(stream),
        Err(err) => Err(TurnError::from_anyhow(err)),
    }
}

/// Sleeps for `limit`, or never when there is no limit.
async fn sleep_or_pending(limit: Option<Duration>) {
    match limit {
        Some(limit) => tokio::time::sleep(limit).await,
        None => std::future::pending().await,
    }
}

/// Retryable timeout for a provider request that made no content progress
/// within `limit`.
fn stream_idle_error(limit: Duration) -> crate::providers::ProviderError {
    crate::providers::ProviderError::timeout(format!(
        "Provider stream stalled: no output for {}s",
        limit.as_secs()
    ))
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
#[allow(clippy::too_many_arguments)]
async fn consume_stream_with_start(
    mut stream: ProviderStream,
    prior_messages: &[ChatMessage],
    sender: &EventSender,
    cancel: Option<&CancellationToken>,
    model: &str,
    provider: &str,
    request_start: RequestSpanStart,
    idle_timeout: Option<Duration>,
) -> std::result::Result<StreamState, (TurnError, StreamState)> {
    let RequestSpanStart {
        instant: request_started_at,
        wall: request_started_at_wall,
    } = request_start;
    let mut state = StreamState::new(model.to_string());
    state.provider = provider.to_string();
    state.request_started_at = request_started_at;
    state.request_started_at_wall = request_started_at_wall;
    // One no-progress budget per request: the wait for the stream to open
    // counts against it. Tokio's clock, so paused-time tests can drive it.
    let now = tokio::time::Instant::now();
    let mut last_progress = now.checked_sub(request_started_at.elapsed()).unwrap_or(now);

    loop {
        if interrupt::is_interrupted() || cancel.is_some_and(CancellationToken::is_cancelled) {
            // User interruption is terminal, not a transparent retry: bill
            // the partial attempt's buffered usage before returning.
            state.flush_pending_usage(sender);
            let turn = std::mem::take(&mut state.turn);
            return Err((interrupted_turn_from_stream(prior_messages, turn), state));
        }
        // Checked on every pass, not only on a quiet poll, so a stream that
        // pings faster than the poll interval still trips it.
        if let Some(limit) = idle_timeout
            && last_progress.elapsed() >= limit
        {
            tracing::warn!(
                elapsed_ms = request_started_at.elapsed().as_millis(),
                idle_secs = limit.as_secs(),
                visible = state.emitted_visible_content,
                "Provider stream stalled"
            );
            return Err((TurnError::Provider(stream_idle_error(limit)), state));
        }
        let event = match timeout(STREAM_POLL_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(event))) => {
                if !matches!(event, StreamEvent::Ping) {
                    last_progress = tokio::time::Instant::now();
                }
                event
            }
            Ok(Some(Err(err))) => {
                tracing::warn!(
                    elapsed_ms = request_started_at.elapsed().as_millis(),
                    error = %zdx_types::log_field(&err.message, 200),
                    "Provider stream failed"
                );
                return Err((TurnError::Provider(err), state));
            }
            Ok(None) => {
                // EOF without an explicit `MessageCompleted`: defensive
                // flush so any buffered usage from a `MessageStart` /
                // `MessageDelta` tick that hadn't yet hit a visible event
                // still reaches downstream consumers on success. This is the
                // request's terminal usage event, so it carries per-request
                // latency.
                state.flush_final_usage(sender);
                tracing::debug!(
                    elapsed_ms = request_started_at.elapsed().as_millis(),
                    stop_reason = state.stop_reason.as_deref().unwrap_or("-"),
                    "Provider stream finished"
                );
                return Ok(state);
            }
            Err(_) => continue,
        };
        if let Err(err) = handle_stream_event(event, sender, &mut state) {
            return Err((err, state));
        }
        if let Some(reason) = state.output_loop {
            // The model was repeating itself instead of finishing. Stop
            // reading the stream and keep what arrived; the caller decides
            // whether to retry once at a lower thinking level or close the
            // turn with a notice.
            tracing::warn!(
                elapsed_ms = request_started_at.elapsed().as_millis(),
                reason = ?reason,
                "Aborting turn: output loop detected"
            );
            state.truncate_looped_output();
            state.flush_final_usage(sender);
            return Ok(state);
        }
    }
}

#[cfg(test)]
async fn consume_stream(
    stream: ProviderStream,
    prior_messages: &[ChatMessage],
    sender: &EventSender,
    cancel: Option<&CancellationToken>,
    model: &str,
    provider: &str,
    request_started_at: Instant,
) -> std::result::Result<StreamState, (TurnError, StreamState)> {
    consume_stream_with_start(
        stream,
        prior_messages,
        sender,
        cancel,
        model,
        provider,
        RequestSpanStart {
            instant: request_started_at,
            wall: crate::core::thread_persistence::chrono_timestamp(),
        },
        None,
    )
    .await
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
            state.turn.ensure_text_part_mut(index).text.push_str(&text);
            state.flush_pending_usage(sender);
            sender.send(AgentEvent::AssistantDelta { text });
            state.mark_visible_content();
            if state.output_loop.is_none()
                && state
                    .turn
                    .find_text_mut(index)
                    .is_some_and(|part| repeats_short_lines(&part.text, TEXT_REPETITION))
            {
                state.output_loop = Some(OutputLoop::TextRepetition);
            }
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
            state.mark_visible_content();
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
                state.mark_visible_content();
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
                state.mark_visible_content();
            }
            if state.output_loop.is_none() {
                state.output_loop = state.check_output_loop();
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
                state.mark_visible_content();
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
    /// Steers added after this round's tool results.
    steered: usize,
}

fn close_steer(options: &AgentOptions) {
    if let Some(source) = &options.steer {
        source.close();
    }
}

/// Adds pending steers after a complete tool round. Each becomes its own
/// user message after the tool results, marked as a steer so it stays input
/// of the running turn (reasoning replay, trajectories, replay notes).
fn inject_steers(
    messages: &mut Vec<ChatMessage>,
    steer: Option<&crate::core::steer::SteerSource>,
    sender: &EventSender,
) -> usize {
    let Some(source) = steer else {
        return 0;
    };
    let items = source.take();
    for item in &items {
        messages.push(ChatMessage::steer(item.text.clone()));
        sender.send(AgentEvent::UserMessageInjected {
            id: item.id.clone(),
            source: item.source.clone(),
            text: item.text.clone(),
        });
    }
    if !items.is_empty() {
        tracing::info!(count = items.len(), "Steer injected");
    }
    items.len()
}

#[tracing::instrument(name = "tool_turn", skip_all)]
#[allow(clippy::too_many_arguments)]
async fn process_tool_turn(
    messages: &mut Vec<ChatMessage>,
    turn: &mut AssistantTurnBuilder,
    setup: &RunTurnSetup,
    sender: &EventSender,
    cancel: Option<&CancellationToken>,
    prior_message_count: usize,
    run_guard: Option<&crate::agent_activity::RunGuard>,
    steer: Option<&crate::core::steer::SteerSource>,
) -> TurnResult<ToolTurnStats> {
    let finalized = std::mem::take(turn).finalize();
    let executable_count = finalized.executable.len();
    let malformed_count = finalized.malformed_results.len();
    tracing::debug!(
        executable = executable_count,
        malformed = malformed_count,
        tools = %finalized
            .executable
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(","),
        "Executing tool calls"
    );
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
        run_guard,
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

    // Safe point: the whole tool round is done and nothing is in flight.
    let steered = inject_steers(messages, steer, sender);

    // Emit a non-terminal checkpoint so persistence flushes the new turn
    // suffix (assistant blocks + tool_results + steers) without waiting for
    // the terminal `TurnFinished`. Long tool loops persist incrementally between
    // tool turns. UI consumers can ignore this; they get live state from
    // streaming events.
    sender.send(AgentEvent::TurnCheckpoint {
        messages: messages.clone(),
        prior_message_count,
    });

    Ok(ToolTurnStats {
        executable: executable_count,
        malformed: malformed_count,
        steered,
    })
}

fn finalize_non_tool_turn(
    messages: &mut Vec<ChatMessage>,
    turn: AssistantTurnBuilder,
    sender: &EventSender,
    prior_message_count: usize,
    offers_dispatch: bool,
) -> (String, Vec<ChatMessage>) {
    let mut finalized = turn.finalize();
    // Phantom-dispatch guard: in a turn that can start workers, an answer
    // that says work started while this turn made no dispatch call gets a
    // correction appended. The model's text is kept as written.
    if offers_dispatch
        && !has_worker_dispatch(&messages[prior_message_count.min(messages.len())..])
        && has_dispatch_claim(&finalized.final_text)
    {
        tracing::warn!(
            "Answer claims work started, but this turn dispatched nothing; appended a correction"
        );
        let correction = format!("\n\n{UNBACKED_DISPATCH_CORRECTION}");
        finalized.final_text.push_str(&correction);
        finalized.blocks.push(ChatContentBlock::text(correction));
    }
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
            context: None,
            context_key: None,
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
            context: None,
            context_key: None,
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
            duration_ms: None,
            started_at: None,
            completed_at: None,
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
        Some("max_tokens") => (
            NoticeKind::OutputLimit,
            "Generation stopped at the output token limit. The response is incomplete; continue or retry for the rest.",
            "stop_reason=max_tokens",
        ),
        _ => return,
    };
    sender.send(AgentEvent::Notice {
        kind,
        message: message.to_string(),
        details: Some(details.to_string()),
    });
}

struct CompletedTool {
    idx: usize,
    id: String,
    output: ToolOutput,
    result: ToolResult,
    duration_ms: Option<u64>,
    started_at: String,
    completed_at: String,
}

#[derive(Clone)]
struct ToolSpanStart {
    wall: String,
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
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn execute_tools_async(
    tool_uses: &[ToolUse],
    ctx: &ToolContext,
    enabled_tools: &HashSet<String>,
    sender: &EventSender,
    tool_registry: &ToolRegistry,
    cancel: Option<&CancellationToken>,
    run_guard: Option<&crate::agent_activity::RunGuard>,
) -> Vec<ToolResult> {
    let mut join_set: JoinSet<CompletedTool> = JoinSet::new();
    let mut results: Vec<Option<(ToolOutput, ToolResult)>> = vec![None; tool_uses.len()];
    let mut completed: HashSet<usize> = HashSet::new();
    let tool_cancel = CancellationToken::new();
    let tool_starts: Vec<_> = tool_uses
        .iter()
        .map(|_| ToolSpanStart {
            wall: crate::core::thread_persistence::chrono_timestamp(),
        })
        .collect();

    emit_tool_started_events(tool_uses, &tool_starts, sender, run_guard);

    for (i, tu) in tool_uses.iter().enumerate() {
        // Clone for 'static requirement
        let tu = tu.clone();
        let mut ctx = ctx.clone();
        ctx.event_sender = Some(sender.clone());
        ctx.tool_use_id = Some(tu.id.clone());
        ctx.cancel_token = Some(tool_cancel.clone());
        let enabled_tools = enabled_tools.clone();
        let tool_registry = tool_registry.clone();
        let tool_start = tool_starts[i].clone();

        // Tokio tasks do not inherit the current span, so attach it explicitly
        // or every tool log line loses its turn/thread context.
        let tool_span = tracing::Span::current();
        join_set.spawn(
            async move {
                let (output, result, duration_ms) = tool_registry
                    .execute_tool(&tu.name, &tu.id, &tu.input, &ctx, &enabled_tools)
                    .await;
                CompletedTool {
                    idx: i,
                    id: tu.id.clone(),
                    output,
                    result,
                    duration_ms,
                    started_at: tool_start.wall,
                    completed_at: crate::core::thread_persistence::chrono_timestamp(),
                }
            }
            .instrument(tool_span),
        );
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
                    run_guard,
                    &tool_cancel,
                    &tool_starts,
                ).await;
                break;
            }
            () = wait_for_cancel(cancel) => {
                handle_tool_interrupt(
                    &mut join_set,
                    &mut completed,
                    &mut results,
                    tool_uses,
                    sender,
                    run_guard,
                    &tool_cancel,
                    &tool_starts,
                ).await;
                break;
            }
            task_result = join_set.join_next() => {
                match task_result {
                    Some(Ok(tool)) => {
                        record_tool_completion(sender, &mut completed, &mut results, tool, run_guard);
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

    ordered_tool_results(results)
}

fn ordered_tool_results(results: Vec<Option<(ToolOutput, ToolResult)>>) -> Vec<ToolResult> {
    results
        .into_iter()
        .map(|result| result.expect("all slots should be filled").1)
        .collect()
}

async fn wait_for_cancel(cancel: Option<&CancellationToken>) {
    if let Some(cancel) = cancel {
        cancel.cancelled().await;
    } else {
        future::pending::<()>().await;
    }
}

fn emit_tool_started_events(
    tool_uses: &[ToolUse],
    starts: &[ToolSpanStart],
    sender: &EventSender,
    run_guard: Option<&crate::agent_activity::RunGuard>,
) {
    for tu in tool_uses {
        sender.send(AgentEvent::ToolStarted {
            id: tu.id.clone(),
            name: tu.name.clone(),
        });
    }
    if let Some(guard) = run_guard {
        guard.set_tools_started(
            tool_uses
                .iter()
                .zip(starts)
                .map(|(tu, start)| {
                    let mut active =
                        crate::agent_activity::ActiveToolCall::new(&tu.id, &tu.name, &tu.input);
                    active.started_at.clone_from(&start.wall);
                    active
                })
                .collect(),
        );
    }
}

fn record_tool_completion(
    sender: &EventSender,
    completed: &mut HashSet<usize>,
    results: &mut [Option<(ToolOutput, ToolResult)>],
    tool: CompletedTool,
    run_guard: Option<&crate::agent_activity::RunGuard>,
) {
    completed.insert(tool.idx);
    if let Some(guard) = run_guard {
        guard.set_tool_finished(&tool.id, tool.output.is_ok(), tool.duration_ms);
    }
    sender.send(AgentEvent::ToolCompleted {
        id: tool.id,
        result: tool.output.clone(),
        duration_ms: tool.duration_ms,
        started_at: Some(tool.started_at),
        completed_at: Some(tool.completed_at),
    });
    results[tool.idx] = Some((tool.output, tool.result));
}

#[allow(clippy::too_many_arguments)]
async fn handle_tool_interrupt(
    join_set: &mut JoinSet<CompletedTool>,
    completed: &mut HashSet<usize>,
    results: &mut [Option<(ToolOutput, ToolResult)>],
    tool_uses: &[ToolUse],
    sender: &EventSender,
    run_guard: Option<&crate::agent_activity::RunGuard>,
    tool_cancel: &CancellationToken,
    tool_starts: &[ToolSpanStart],
) {
    const TEARDOWN_WAIT: Duration = Duration::from_secs(6);

    tool_cancel.cancel();

    let teardown = tokio::time::sleep(TEARDOWN_WAIT);
    tokio::pin!(teardown);
    loop {
        tokio::select! {
            biased;
            task_result = join_set.join_next() => match task_result {
                Some(Ok(tool)) if !completed.contains(&tool.idx) => {
                    record_tool_completion(sender, completed, results, tool, run_guard);
                }
                Some(_) => {}
                None => break,
            },
            () = &mut teardown => break,
        }
    }

    join_set.abort_all();
    while let Some(task_result) = join_set.join_next().await {
        if let Ok(tool) = task_result
            && !completed.contains(&tool.idx)
        {
            record_tool_completion(sender, completed, results, tool, run_guard);
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
                CompletedTool {
                    idx: i,
                    id: tu.id.clone(),
                    output: abort_output,
                    result: abort_result,
                    duration_ms: None,
                    started_at: tool_starts[i].wall.clone(),
                    completed_at: crate::core::thread_persistence::chrono_timestamp(),
                },
                run_guard,
            );
        }
    }

    if let Some(guard) = run_guard {
        guard.set_tools_started(Vec::new());
    }
}

/// Builds assistant content blocks from accumulated thinking, reasoning, text, and tool uses.
#[cfg(test)]
mod tests {
    use tokio::time::{Duration, timeout};

    use super::*;

    fn bash_policy_options(
        root: &std::path::Path,
        tools: &[&str],
        background_commands: bool,
        tool_timeout: Option<Duration>,
    ) -> AgentOptions {
        AgentOptions {
            conversation_id: None,
            root: root.to_path_buf(),
            tool_config: ToolConfig::new(
                ToolRegistry::builtins(),
                ToolSelection::Explicit(tools.iter().map(ToString::to_string).collect()),
            )
            .with_background_commands(background_commands),
            surface: Some("exec".to_string()),
            text_verbosity: None,
            service_tier: None,
            activity_kind: None,
            activity_parent_thread_id: None,
            activity_subagent_name: None,
            tool_timeout,
            steer: None,
            run_lease: None,
        }
    }

    fn names(tools: &[ToolDefinition]) -> Vec<String> {
        let mut names: Vec<String> = tools.iter().map(|t| t.name.to_lowercase()).collect();
        names.sort();
        names
    }

    fn bash_def(tools: &[ToolDefinition]) -> &ToolDefinition {
        tools
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case("bash"))
            .expect("bash present")
    }

    /// The tool list, the `Bash` schema, and the tool context all come from
    /// the same resolved policy: without background execution the management
    /// tools are gone even when requested, `Bash` has no `background`
    /// property, and its text claims no implicit limit. The run deadline is
    /// left to the tool context (`timeout`), not folded into the policy.
    #[test]
    fn tools_follow_a_run_without_background_execution() {
        let home = crate::test_support::temp_zdx_home();
        let mut config = Config::default();
        config.subagents.enabled = false;
        config.providers.anthropic.api_key = Some("test-key".to_string());
        config.model = "anthropic:claude-sonnet-4-6".to_string();

        let options = bash_policy_options(
            home.path(),
            &["bash", "read", "background_output", "background_kill"],
            false,
            Some(Duration::from_secs(20)),
        );
        let setup = build_run_turn_setup(&config, &options, None, &[], None).unwrap();

        assert_eq!(names(&setup.tools), vec!["bash", "read"]);
        let enabled: Vec<String> = setup
            .enabled_tools
            .iter()
            .map(|t| t.to_lowercase())
            .collect();
        assert!(!enabled.iter().any(|t| t.starts_with("background_")));
        let bash = bash_def(&setup.tools);
        assert!(bash.input_schema["properties"].get("background").is_none());
        assert!(
            bash.description
                .contains("No time limit applies unless you set timeout_secs"),
            "{}",
            bash.description
        );
        assert_eq!(
            setup.tool_ctx.background_execution,
            BackgroundExecution::Disabled
        );
        assert_eq!(setup.tool_ctx.timeout, Some(Duration::from_secs(20)));
    }

    /// With background execution, a run that has `bash` always gets the
    /// tools that read and stop its jobs, so no job it starts is out of its
    /// reach; a run without `bash` is left as selected.
    #[test]
    fn tools_follow_a_run_with_background_execution() {
        let home = crate::test_support::temp_zdx_home();
        let mut config = Config::default();
        config.subagents.enabled = false;

        let with_bash = resolve_active_tools(
            &config,
            &bash_policy_options(home.path(), &["bash"], true, None),
            ProviderKind::Anthropic,
        );
        assert_eq!(
            names(&with_bash),
            vec!["background_kill", "background_output", "bash"]
        );
        assert!(
            bash_def(&with_bash).input_schema["properties"]
                .get("background")
                .is_some()
        );

        let without_bash = resolve_active_tools(
            &config,
            &bash_policy_options(home.path(), &["read"], true, None),
            ProviderKind::Anthropic,
        );
        assert_eq!(names(&without_bash), vec!["read"]);
    }

    fn invoke_def(tools: &[ToolDefinition]) -> &ToolDefinition {
        tools
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case("invoke_subagent"))
            .expect("invoke_subagent present")
    }

    /// `invoke_subagent` follows the same policy as `Bash`: with background
    /// execution it advertises `background` and brings the management tools
    /// even without `bash`; without it, it keeps its blocking schema and the
    /// management tools are absent.
    #[test]
    fn invoke_subagent_follows_the_background_policy() {
        let home = crate::test_support::temp_zdx_home();
        let mut config = Config::default();
        config.subagents.enabled = true;

        let enabled = resolve_active_tools(
            &config,
            &bash_policy_options(home.path(), &["invoke_subagent", "read"], true, None),
            ProviderKind::Anthropic,
        );
        assert_eq!(
            names(&enabled),
            vec![
                "background_kill",
                "background_output",
                "invoke_subagent",
                "read"
            ]
        );
        let invoke = invoke_def(&enabled);
        assert_eq!(
            invoke.input_schema["properties"]["background"]["type"], "boolean",
            "background option missing"
        );
        assert!(invoke.description.contains("background_output"));

        let disabled = resolve_active_tools(
            &config,
            &bash_policy_options(
                home.path(),
                &["invoke_subagent", "read", "background_output"],
                false,
                None,
            ),
            ProviderKind::Anthropic,
        );
        assert_eq!(names(&disabled), vec!["invoke_subagent", "read"]);
        let invoke = invoke_def(&disabled);
        assert!(
            invoke.input_schema["properties"]
                .get("background")
                .is_none()
        );
        assert!(!invoke.description.contains("background_output"));
    }

    /// The turn owns a task set from its first tool call, and keeps the same
    /// one when the setup is rebuilt for a lower thinking level.
    #[test]
    fn rebuilt_setup_keeps_the_turn_task_set() {
        let home = crate::test_support::temp_zdx_home();
        let mut config = Config::default();
        config.subagents.enabled = false;
        config.providers.anthropic.api_key = Some("test-key".to_string());
        config.model = "anthropic:claude-sonnet-4-6".to_string();
        config.thinking_level = ThinkingLevel::High;
        let options = bash_policy_options(home.path(), &["bash"], true, None);

        let mut setup = build_run_turn_setup(&config, &options, None, &[], None).unwrap();
        let (_guard, tasks) = crate::tools::subagent_job::TurnTasksGuard::new();
        setup.tool_ctx.turn_tasks = Some(Arc::clone(&tasks));
        let rebuilt = rebuild_for_lower_thinking(&setup, &config, &options, None, &[])
            .expect("a lower thinking level exists");
        assert!(Arc::ptr_eq(
            rebuilt.tool_ctx.turn_tasks.as_ref().unwrap(),
            &tasks
        ));
    }

    #[tokio::test]
    async fn main_turn_uses_model_thinking_suffix() {
        let home = crate::test_support::temp_zdx_home();
        let path = home.path().join("config.toml");
        let options = AgentOptions {
            conversation_id: None,
            root: home.path().to_path_buf(),
            tool_config: ToolConfig::new(ToolRegistry::new(), ToolSelection::Explicit(Vec::new())),
            surface: None,
            text_verbosity: None,
            service_tier: None,
            activity_kind: None,
            activity_parent_thread_id: None,
            activity_subagent_name: None,
            tool_timeout: None,
            steer: None,
            run_lease: None,
        };
        for (suffix, expected) in [
            ("@high", ThinkingLevel::High),
            ("@off", ThinkingLevel::Off),
            ("", ThinkingLevel::Off),
        ] {
            std::fs::write(
                &path,
                format!("model = \"anthropic:claude-sonnet-4-6{suffix}\"\n"),
            )
            .unwrap();
            let mut config = Config::load_from(&path).unwrap();
            config.providers.anthropic.api_key = Some("test-key".to_string());
            config.subagents.enabled = false;
            let setup = build_run_turn_setup(&config, &options, None, &[], None).unwrap();
            assert_eq!(setup.thinking_level, expected);
            assert_eq!(setup.model, "claude-sonnet-4-6");
            assert_eq!(setup.tool_ctx.thinking_level, Some(expected));
        }
    }
    use crate::providers::gemini::{GeminiClient, GeminiConfig};

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
    fn stop_reason_notice_emits_for_output_limit() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = EventSender::new(tx);

        emit_stop_reason_notice(Some("max_tokens"), &sender);

        let evt = rx.try_recv().expect("expected an event");
        match &*evt {
            AgentEvent::Notice { kind, message, .. } => {
                assert_eq!(*kind, NoticeKind::OutputLimit);
                assert!(message.contains("output token limit"), "got: {message}");
            }
            other => panic!("expected Notice, got {other:?}"),
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

    /// Builds `count` distinct lines for repetition-detector fixtures.
    fn distinct_lines(count: usize, line: impl Fn(usize) -> String) -> String {
        (0..count).fold(String::new(), |mut acc, i| {
            acc.push_str(&line(i));
            acc
        })
    }

    fn reasoning_state(text: &str) -> StreamState {
        let mut state = StreamState::new("test-model".to_string());
        state.turn.push_reasoning(ThinkingBuilder {
            index: 0,
            text: text.to_string(),
            signature: String::new(),
            signature_provider: None,
            replay: None,
            had_delta: true,
        });
        state
    }

    #[test]
    fn reasoning_repetition_detects_the_filler_loop() {
        let loop_text =
            "Let me run.\n\nLet me go.\n\nLet me do it.\n\nLet me execute.\n\n".repeat(40);

        assert!(reasoning_repeats(&loop_text));
    }

    #[test]
    fn reasoning_repetition_ignores_genuine_deliberation() {
        let text = "The store module owns the registry, so the uninstall path has to \
                    update it before the SPA reloads.\n\
                    That means the bridge needs a snapshot call after the write.\n\
                    Let me check whether the existing test covers the empty case.\n"
            .repeat(20);

        assert!(!reasoning_repeats(&text));
    }

    #[test]
    fn reasoning_repetition_needs_a_full_window() {
        assert!(!reasoning_repeats("Let me run.\n\nLet me go.\n\n"));
    }

    #[test]
    fn text_repetition_detects_the_filler_loop() {
        assert!(repeats_short_lines(
            &"Let me run.\n\nLet me go.\n\nLet me do it.\n\nLet me execute.\n\n".repeat(200),
            TEXT_REPETITION
        ));
    }

    /// The answer guard must not fire on output that is repetitive but real:
    /// tables, similar log lines, generated code, and prose that merely shares
    /// a shape. No line repeats ten times in any of these.
    #[test]
    fn text_repetition_ignores_legitimately_repetitive_output() {
        let table = format!(
            "| id | name | status |\n|---|---|---|\n{}",
            distinct_lines(200, |i| format!("| {i} | row{i} | ok |\n"))
        );
        let log_lines = distinct_lines(200, |i| {
            format!(
                "2026-09-18T10:{:02}:00Z INFO handler={i} processed=ok\n",
                i % 60
            )
        });
        let code = distinct_lines(200, |i| format!(".c{i} {{ color: red; }}\n"));
        let prose_lines = distinct_lines(80, |i| {
            format!("A fairly long line of prose number {i} exceeding forty chars.\n")
        });
        // Nine repeats is below the repeat threshold even though the window is
        // full of short lines.
        let nine_repeats = format!(
            "{}distinct filler content to fill the window out to four thousand characters\n",
            "same short line\n".repeat(9)
        )
        .repeat(40);

        for (name, text) in [
            ("table", table),
            ("log lines", log_lines),
            ("code", code),
            ("long lines", prose_lines),
            ("nine repeats", nine_repeats),
        ] {
            assert!(
                !repeats_short_lines(&text, TEXT_REPETITION),
                "{name} must not be flagged as a loop"
            );
        }
    }

    #[test]
    fn lowered_thinking_level_steps_down_and_stops_at_low() {
        assert_eq!(
            lowered_thinking_level(ThinkingLevel::Max),
            Some(ThinkingLevel::XHigh)
        );
        assert_eq!(
            lowered_thinking_level(ThinkingLevel::High),
            Some(ThinkingLevel::Medium)
        );
        assert_eq!(
            lowered_thinking_level(ThinkingLevel::Medium),
            Some(ThinkingLevel::Low)
        );
        assert_eq!(lowered_thinking_level(ThinkingLevel::Low), None);
        assert_eq!(lowered_thinking_level(ThinkingLevel::Off), None);
    }

    fn tool_turn(name: &str, input: &str, result: &str) -> Vec<ChatMessage> {
        tool_round(&[(name, input, result)])
    }

    /// One tool round with several calls, each `(name, input, result)`.
    fn tool_round(calls: &[(&str, &str, &str)]) -> Vec<ChatMessage> {
        let id = |index: usize| format!("toolu_{index}");
        vec![
            ChatMessage::assistant_blocks(
                calls
                    .iter()
                    .enumerate()
                    .map(|(index, (name, input, _))| ChatContentBlock::ToolUse {
                        id: id(index),
                        name: (*name).to_string(),
                        input: serde_json::json!({ "arg": input }),
                        id_origin: zdx_types::IdOrigin::Real,
                        replay: None,
                    })
                    .collect(),
            ),
            ChatMessage::tool_results(
                calls
                    .iter()
                    .enumerate()
                    .map(|(index, (_, _, result))| {
                        ToolResult::from_output(
                            id(index),
                            &ToolOutput::success(serde_json::json!({ "out": result })),
                        )
                    })
                    .collect(),
            ),
        ]
    }

    #[test]
    fn tool_loop_tracker_trips_after_three_identical_calls() {
        let mut tracker = ToolLoopTracker::default();

        assert!(!tracker.record(&tool_turn("read", "a.txt", "same")));
        assert!(!tracker.record(&tool_turn("read", "a.txt", "same")));
        assert!(tracker.record(&tool_turn("read", "a.txt", "same")));
    }

    /// A real retry loop changes the result each time, so it never trips.
    #[test]
    fn tool_loop_tracker_ignores_changing_results() {
        let mut tracker = ToolLoopTracker::default();

        assert!(!tracker.record(&tool_turn("bash", "cargo test", "fail")));
        assert!(!tracker.record(&tool_turn("bash", "cargo test", "fail")));
        assert!(!tracker.record(&tool_turn("bash", "cargo test", "pass")));
        assert!(!tracker.record(&tool_turn("bash", "cargo test", "pass")));
    }

    #[test]
    fn tool_loop_tracker_distinguishes_inputs_and_tools() {
        let mut tracker = ToolLoopTracker::default();

        assert!(!tracker.record(&tool_turn("read", "a.txt", "same")));
        assert!(!tracker.record(&tool_turn("read", "b.txt", "same")));
        assert!(!tracker.record(&tool_turn("grep", "a.txt", "same")));
    }

    /// Regression: thread 636c69e9. A passing typecheck prints the same output
    /// every time, but each run follows new edits, so it must never trip.
    #[test]
    fn tool_loop_tracker_ignores_identical_checks_between_edits() {
        let mut tracker = ToolLoopTracker::default();

        assert!(!tracker.record(&tool_turn("bash", "typecheck", "tsc ok")));
        for round in 0..6 {
            let patch = format!("patch {round}");
            assert!(!tracker.record(&tool_turn("apply_patch", &patch, "ok")));
            assert!(!tracker.record(&tool_round(&[
                ("bash", "build", "built"),
                ("bash", "typecheck", "tsc ok"),
            ])));
        }
    }

    /// Two calls that alternate with unchanged results are a loop too. The
    /// window opens once both have been seen, so it trips on the sixth round.
    #[test]
    fn tool_loop_tracker_trips_on_alternating_cycle() {
        let mut tracker = ToolLoopTracker::default();

        assert!(!tracker.record(&tool_turn("read", "a.txt", "a")));
        assert!(!tracker.record(&tool_turn("read", "b.txt", "b")));
        assert!(!tracker.record(&tool_turn("read", "a.txt", "a")));
        assert!(!tracker.record(&tool_turn("read", "b.txt", "b")));
        assert!(!tracker.record(&tool_turn("read", "a.txt", "a")));
        assert!(tracker.record(&tool_turn("read", "b.txt", "b")));
    }

    /// The lower-thinking retry gets a fresh window but still catches a loop
    /// that continues after it.
    #[test]
    fn tool_loop_tracker_reset_gives_fresh_counts() {
        let mut tracker = ToolLoopTracker::default();

        assert!(!tracker.record(&tool_turn("read", "a.txt", "same")));
        assert!(!tracker.record(&tool_turn("read", "a.txt", "same")));
        assert!(tracker.record(&tool_turn("read", "a.txt", "same")));

        tracker.reset_counts();
        assert!(!tracker.record(&tool_turn("read", "a.txt", "same")));
        assert!(!tracker.record(&tool_turn("read", "a.txt", "same")));
        assert!(tracker.record(&tool_turn("read", "a.txt", "same")));
    }

    #[test]
    fn truncate_looped_output_shrinks_text_too() {
        let mut state = StreamState::new("test-model".to_string());
        let body = "Let me run.\n\n".repeat(2_000);
        state.turn.ensure_text_part_mut(0).text = body.clone();

        state.truncate_looped_output();

        let text = &state.turn.find_text_mut(0).expect("text part").text;
        assert!(text.len() < body.len());
        assert!(text.starts_with("Let me run."));
        assert!(text.contains("bytes truncated"));
    }

    #[test]
    fn runaway_guard_trips_on_budget_without_committed_output() {
        let state = reasoning_state(&"x".repeat(RUNAWAY_REASONING_CHARS + 1));

        assert_eq!(state.check_output_loop(), Some(OutputLoop::ReasoningBudget));
    }

    #[test]
    fn runaway_guard_stands_down_once_text_is_emitted() {
        let mut state = reasoning_state(&"x".repeat(RUNAWAY_REASONING_CHARS + 1));
        state.turn.ensure_text_part_mut(1).text = "here is the answer".to_string();

        assert_eq!(state.check_output_loop(), None);
    }

    #[test]
    fn runaway_guard_stands_down_once_a_tool_call_starts() {
        let mut state = reasoning_state(&"x".repeat(RUNAWAY_REASONING_CHARS + 1));
        state.turn.push_tool_use(ToolUseBuilder {
            index: 1,
            id: "toolu_1".to_string(),
            name: "read".to_string(),
            input_json: "{}".to_string(),
            input_preview_len: 0,
            id_origin: zdx_types::IdOrigin::Real,
            replay: None,
        });

        assert_eq!(state.check_output_loop(), None);
    }

    /// A truncated runaway keeps both ends and drops the replay token, which
    /// no longer matches the surviving text.
    #[test]
    fn truncate_runaway_reasoning_keeps_head_and_tail_and_clears_replay() {
        let mut state = StreamState::new("test-model".to_string());
        let body = "Let me run.\n\n".repeat(2_000);
        state.turn.push_reasoning(ThinkingBuilder {
            index: 0,
            text: body.clone(),
            signature: "sig".to_string(),
            signature_provider: Some(crate::providers::SignatureProvider::Anthropic),
            replay: Some(ReplayToken::Anthropic {
                signature: "sig".to_string(),
            }),
            had_delta: true,
        });

        state.truncate_looped_output();

        let finalized = state.turn.finalize();
        let Some(ChatContentBlock::Reasoning(reasoning)) = finalized.blocks.first() else {
            panic!("expected a reasoning block");
        };
        let text = reasoning.text.as_deref().expect("reasoning text");
        assert!(text.len() < body.len());
        assert!(text.starts_with("Let me run."));
        assert!(text.ends_with("Let me run.\n\n"));
        assert!(text.contains("bytes truncated"));
        assert!(
            reasoning.replay.is_none(),
            "stale signature must be dropped"
        );
    }

    #[test]
    fn finalize_truncates_an_oversized_reasoning_block() {
        let mut turn = AssistantTurnBuilder::new("test-model".to_string());
        turn.push_reasoning(ThinkingBuilder {
            index: 0,
            text: "y".repeat(MAX_PERSISTED_REASONING_CHARS + 1),
            signature: String::new(),
            signature_provider: None,
            replay: None,
            had_delta: true,
        });

        let finalized = turn.finalize();

        let Some(ChatContentBlock::Reasoning(reasoning)) = finalized.blocks.first() else {
            panic!("expected a reasoning block");
        };
        let text = reasoning.text.as_deref().expect("reasoning text");
        assert!(text.len() < MAX_PERSISTED_REASONING_CHARS + 200);
        assert!(text.contains("bytes truncated"));
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
                    ..
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
            matches!(&received[1], AgentEvent::ToolCompleted { id, result, .. }
            if id == "tool1" && result.is_ok())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tool_cancellation_waits_for_foreground_process_cleanup() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("foreground.pid");
        let ctx = ToolContext::new(temp.path().to_path_buf(), None);
        let enabled_tools: HashSet<String> = vec!["bash".to_string()].into_iter().collect();
        let tool_uses = vec![ToolUse {
            id: "tool1".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({
                "command": format!(
                    "trap '' TERM; echo $$ > {}; while :; do sleep 1; done",
                    pidfile.display()
                ),
                "description": "test command"
            }),
            id_origin: zdx_types::IdOrigin::Synthesized,
            replay: None,
        }];
        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let registry = ToolRegistry::builtins();
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            execute_tools_async(
                &tool_uses,
                &ctx,
                &enabled_tools,
                &sender,
                &registry,
                Some(&task_cancel),
                None,
            )
            .await
        });

        let pid: i32 = timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(raw) = std::fs::read_to_string(&pidfile)
                    && let Ok(pid) = raw.trim().parse()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("foreground command did not publish its pid");

        cancel.cancel();
        let results = timeout(Duration::from_secs(3), task)
            .await
            .expect("tool teardown did not complete")
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        let payload: serde_json::Value = serde_json::from_str(
            results[0]
                .content
                .as_text()
                .expect("canceled tool result should be text"),
        )
        .expect("canceled tool result should be JSON");
        assert_eq!(payload["error"]["code"], "canceled");
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }

    #[tokio::test]
    async fn test_execute_tools_runs_todo_write_snapshots() {
        let ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        let enabled_tools: HashSet<String> = vec!["Todo_Write".to_string()].into_iter().collect();
        let tool_uses = vec![
            ToolUse {
                id: "tool1".to_string(),
                name: "Todo_Write".to_string(),
                input: serde_json::json!({
                    "todos": [
                        {"content": "Inspect codebase", "status": "in_progress"}
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
                        {"content": "Inspect codebase", "status": "completed"},
                        {"content": "Ship fix", "status": "in_progress"}
                    ]
                }),
                id_origin: zdx_types::IdOrigin::Synthesized,
                replay: None,
            },
        ];

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let tool_registry = ToolRegistry::builtins();

        let results = execute_tools_async(
            &tool_uses,
            &ctx,
            &enabled_tools,
            &sender,
            &tool_registry,
            None,
            None,
        )
        .await;

        assert_eq!(results.len(), 2);
        assert!(!results[0].is_error);
        assert!(!results[1].is_error);

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|event| (*event).clone())
            .collect();
        let measured_completions = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    AgentEvent::ToolCompleted {
                        duration_ms: Some(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(measured_completions, 2);

        let second = results[1]
            .content
            .as_text()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .expect("Todo_Write result should be valid JSON text");

        // Each call is a whole-list snapshot, so the second result stands alone
        // rather than being merged with the first.
        let todos = second
            .get("data")
            .and_then(|data| data.get("todos"))
            .and_then(|todos| todos.as_array())
            .expect("Todo_Write should return todos array");

        assert_eq!(todos.len(), 2);
        assert_eq!(
            todos[0].get("content").and_then(|c| c.as_str()),
            Some("Inspect codebase")
        );
        assert_eq!(
            todos[0].get("status").and_then(|status| status.as_str()),
            Some("completed")
        );
        assert_eq!(
            todos[1].get("content").and_then(|c| c.as_str()),
            Some("Ship fix")
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
                context: None,
                context_key: None,
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
            context: _,
            context_key: _,
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
    #[test]
    fn strip_invalid_images_replaces_only_the_named_message() {
        let mut messages = vec![
            ChatMessage::user("look"),
            ChatMessage {
                role: "user".to_string(),
                phase: None,
                context: None,
                context_key: None,
                content: MessageContent::Blocks(vec![
                    ChatContentBlock::text("see"),
                    ChatContentBlock::Image {
                        mime_type: "image/png".to_string(),
                        data: "AAAA".to_string(),
                    },
                ]),
            },
            ChatMessage::user("other"),
        ];
        let err = ProviderError::http_status(
            400,
            r#"{"error":{"message":"invalid image data at input[1].content[0]"}}"#,
        );
        assert!(err.is_invalid_image());
        assert!(strip_invalid_images(&mut messages, &err));
        let MessageContent::Blocks(blocks) = &messages[1].content else {
            panic!("expected blocks");
        };
        assert_eq!(
            blocks[1],
            ChatContentBlock::text("[image omitted: invalid image data]")
        );
        // Nothing left to strip: a second pass must not claim success.
        assert!(!strip_invalid_images(&mut messages, &err));
    }

    /// A dispatch call and its result, as one turn records them.
    fn dispatch_messages(tool: &str, result_json: &str) -> Vec<ChatMessage> {
        vec![
            ChatMessage::assistant_blocks(vec![ChatContentBlock::tool_use(
                "call-1",
                tool,
                serde_json::json!({"prompt": "go"}),
            )]),
            ChatMessage::tool_results(vec![ToolResult {
                tool_use_id: "call-1".to_string(),
                content: ToolResultContent::Text(result_json.to_string()),
                is_error: false,
            }]),
        ]
    }

    /// Runs `finalize_non_tool_turn` on one text answer after `history`,
    /// where `prior` messages came before this turn.
    fn finalize_answer(
        history: Vec<ChatMessage>,
        prior: usize,
        answer: &str,
        offers_dispatch: bool,
    ) -> String {
        let mut turn = AssistantTurnBuilder::new(String::new());
        turn.ensure_text_part_mut(0).text = answer.to_string();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = EventSender::new(tx);
        let mut messages = history;
        let (final_text, messages) =
            finalize_non_tool_turn(&mut messages, turn, &sender, prior, offers_dispatch);
        let persisted: String = match &messages.last().expect("assistant message").content {
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    ChatContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect(),
            MessageContent::Text(text) => text.clone(),
        };
        assert_eq!(persisted, final_text, "persisted text matches final text");
        final_text
    }

    #[test]
    fn worker_dispatch_requires_a_thread_id_result() {
        assert!(has_worker_dispatch(&dispatch_messages(
            "Create_Thread",
            r#"{"ok":true,"data":{"thread_id":"w-1"}}"#,
        )));
        assert!(has_worker_dispatch(&dispatch_messages(
            "send_thread_message",
            r#"{"ok":true,"data":{"worker":{"thread_id":"w-1"}}}"#,
        )));
        assert!(has_worker_dispatch(&dispatch_messages(
            "Invoke_Subagent",
            r#"{"ok":true,"data":{"result":"done","child_thread_id":"c-1"}}"#,
        )));

        let mut failed = dispatch_messages(
            "create_thread",
            r#"{"ok":false,"error":{"code":"create_thread_failed","message":"boom"}}"#,
        );
        if let MessageContent::Blocks(blocks) = &mut failed[1].content
            && let ChatContentBlock::ToolResult(result) = &mut blocks[0]
        {
            result.is_error = true;
        }
        assert!(!has_worker_dispatch(&failed));
        assert!(!has_worker_dispatch(&[ChatMessage::user("hi")]));
    }

    #[test]
    fn dispatch_claims_match_observed_phantom_answers() {
        for claim in [
            "**Making Odin the sessiond unit now.**\n\nWorker will check `compositor.toml`.",
            "**Installing the new bundle on r11.**\n\n**What's in it**:\n1. Shell socket",
            "**Retrying the Odin sessiond-unit worker now.**\n\nFresh thread (previous one died).",
            "**Retrying with a fresh worker now.**",
            "**Remoting the new release.**\n\nWorker will build the release on namespace.",
            "**On it — starting `powerd` + `appd` in the kit session.**",
            "**Fixing CreateClient slots now.**",
            "I've started a new worker to handle this. The screenshot was 10 MB.",
            "I'm retrying the remote build now.",
        ] {
            assert!(has_dispatch_claim(claim), "should be a claim: {claim}");
        }
        for not_claim in [
            "No worker was started yet.",
            "**Yes — the remote build is already running.**",
            "**Not sure — likely not running yet.** The retry worker hasn't reported back.",
            "**Handling parallel app packing:** split the 9 apps across boxes.",
            "Worker `3c8c6b54` started at 20:22 and is still building on box n53u.",
            "I can start a fresh worker for it if you want.",
            "**Running for 41 min — in the package build stage.**",
            "**Install done and verified — then replaced by another worker's install.**",
            "The investigation worker I started is tracing that path.",
            "**No. It's a stopgap.** Making it the default is okay for now because Odin is validated.",
            "- The transcript has text saying a worker started, but zero new thread ids.",
            "**Exactly.** The pinned image covers the whole build. Running it now would take 40 min? No.",
        ] {
            assert!(
                !has_dispatch_claim(not_claim),
                "should not be a claim: {not_claim}"
            );
        }
    }

    #[test]
    fn unbacked_claim_in_a_long_orchestrator_thread_gets_a_correction() {
        // An older turn dispatched a worker; this turn dispatched nothing.
        let mut history = vec![ChatMessage::user("Build the image")];
        history.extend(dispatch_messages(
            "create_thread",
            r#"{"ok":true,"data":{"thread_id":"w-1"}}"#,
        ));
        history.push(ChatMessage::assistant_blocks(vec![ChatContentBlock::text(
            "Started the image worker.",
        )]));
        history.push(ChatMessage::user("Make Odin the sessiond unit"));
        assert!(
            has_worker_dispatch(&history),
            "the whole thread has a dispatch"
        );
        let prior = history.len();

        let answer = "**Making Odin the sessiond unit now.**\n\n## Plan\n\nWorker will  check it.";
        let final_text = finalize_answer(history, prior, answer, true);
        assert_eq!(
            final_text,
            format!("{answer}\n\n{UNBACKED_DISPATCH_CORRECTION}"),
            "the model's text is kept byte for byte, then the correction"
        );
    }

    #[test]
    fn dispatch_in_this_turn_backs_the_claim() {
        let mut history = vec![ChatMessage::user("Make Odin the sessiond unit")];
        let prior = history.len();
        history.extend(dispatch_messages(
            "create_thread",
            r#"{"ok":true,"data":{"thread_id":"w-2"}}"#,
        ));
        let answer = "**Making Odin the sessiond unit now.**";
        assert_eq!(finalize_answer(history, prior, answer, true), answer);
    }

    #[test]
    fn invoke_subagent_backs_a_subagent_claim() {
        let mut history = vec![ChatMessage::user("Review this")];
        let prior = history.len();
        history.extend(dispatch_messages(
            "invoke_subagent",
            r#"{"ok":true,"data":{"result":"done","child_thread_id":"c-1"}}"#,
        ));
        let answer = "I've started a subagent to review it. It found 2 issues.";
        assert_eq!(finalize_answer(history, prior, answer, true), answer);
    }

    #[test]
    fn turns_without_create_thread_are_left_untouched() {
        // Worker, helper, and job turns do not offer create_thread: their
        // reports about other threads' work are never rewritten.
        let history = vec![ChatMessage::user("Is it still installing?")];
        let prior = history.len();
        let answer = "No.  Nothing is installing on r11 now.\n**Install worker**\n- Worker \
                      `3c8c6b54` is still running.\n\n**Retrying the build now.** I've started a \
                      new worker.";
        assert_eq!(finalize_answer(history, prior, answer, false), answer);

        let mut tools: HashSet<String> = ["bash", "read", "Invoke_Subagent"]
            .into_iter()
            .map(str::to_string)
            .collect();
        assert!(!offers_worker_dispatch(&tools));
        tools.insert("Create_Thread".to_string());
        assert!(offers_worker_dispatch(&tools));
    }

    #[test]
    fn strip_invalid_images_falls_back_and_names_the_tool_result_path() {
        let result = ToolResult {
            tool_use_id: "tool-1".to_string(),
            content: ToolResultContent::Blocks(vec![
                ToolResultBlock::Text {
                    text: r#"{"ok":true,"data":{"file_path":"/tmp/menu-crop.png"}}"#.to_string(),
                },
                ToolResultBlock::Image {
                    mime_type: "image/png".to_string(),
                    data: "AAAA".to_string(),
                },
            ]),
            is_error: false,
        };
        let mut messages = vec![ChatMessage::tool_results(vec![result])];
        let err = ProviderError::http_status(400, "invalid image data");
        assert!(strip_invalid_images(&mut messages, &err));
        let MessageContent::Blocks(blocks) = &messages[0].content else {
            panic!("expected blocks");
        };
        let ChatContentBlock::ToolResult(stripped) = &blocks[0] else {
            panic!("expected tool result");
        };
        let ToolResultContent::Blocks(inner) = &stripped.content else {
            panic!("expected image blocks");
        };
        assert_eq!(
            inner[1],
            ToolResultBlock::Text {
                text: "[image omitted: invalid image data at /tmp/menu-crop.png]".to_string()
            }
        );
    }

    #[tokio::test]
    async fn test_consume_stream_returns_state_on_midstream_error() {
        use futures_util::stream;

        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);

        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![Err(
            ProviderError::api_error("overloaded_error", "API is temporarily overloaded"),
        )];
        let provider_stream: ProviderStream = Box::pin(stream::iter(events));

        let result = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;
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
        let transport_err =
            ProviderError::transport("SSE stream network error: connection reset by peer");
        let events: Vec<crate::providers::ProviderResult<StreamEvent>> = vec![Err(transport_err)];
        let provider_stream: ProviderStream = Box::pin(stream::iter(events));

        let result = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;
        let Err((err, state)) = result else {
            panic!("stream should error out");
        };
        assert!(matches!(
            &err,
            TurnError::Provider(p)
                if p.is_retryable()
                    && p.kind == ProviderErrorKind::Transport
                    && p.message.contains("network error")
        ));
        assert!(
            can_transparently_retry_stream(&state),
            "transport error before any emitted events must remain retry-safe"
        );
    }

    /// Runs `consume_stream_with_start` with a no-progress limit.
    async fn consume_with_idle(
        stream: ProviderStream,
        cancel: Option<&CancellationToken>,
        idle: Duration,
    ) -> std::result::Result<StreamState, (TurnError, StreamState)> {
        let (tx, _rx) = create_event_channel();
        let sender = EventSender::new(tx);
        consume_stream_with_start(
            stream,
            &[],
            &sender,
            cancel,
            "",
            "",
            RequestSpanStart {
                instant: Instant::now(),
                wall: crate::core::thread_persistence::chrono_timestamp(),
            },
            Some(idle),
        )
        .await
    }

    fn reasoning_events() -> Vec<crate::providers::ProviderResult<StreamEvent>> {
        vec![
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
        ]
    }

    fn assert_stall_error(err: &TurnError) {
        assert!(
            matches!(
                err,
                TurnError::Provider(p)
                    if p.kind == crate::providers::ProviderErrorKind::Timeout
                        && p.is_retryable()
                        && p.message.contains("stalled")
            ),
            "expected a retryable stall timeout, got {err:?}"
        );
    }

    /// The 2026-10 incident shape: output arrives, then the stream goes
    /// silent without closing. The turn must end with a stall error instead of
    /// waiting forever, and because output was already shown it must not be
    /// retried transparently.
    #[tokio::test(start_paused = true)]
    async fn stream_idle_timeout_ends_a_stream_that_stalls_after_output() {
        use futures_util::stream;

        let started = tokio::time::Instant::now();
        let provider_stream: ProviderStream =
            Box::pin(stream::iter(reasoning_events()).chain(stream::pending()));

        let result = consume_with_idle(provider_stream, None, Duration::from_mins(5)).await;

        let Err((err, state)) = result else {
            panic!("a stalled stream must fail");
        };
        assert_stall_error(&err);
        assert!(started.elapsed() >= Duration::from_mins(5));
        assert!(started.elapsed() < Duration::from_secs(301));
        assert!(
            !can_transparently_retry_stream(&state),
            "output was already emitted, so the stall must not be retried transparently"
        );
    }

    /// Keepalive pings are not progress: a stream that only pings (faster
    /// than the poll interval) still trips the limit, and since nothing was
    /// shown it stays eligible for the transparent retry.
    #[tokio::test(start_paused = true)]
    async fn stream_idle_timeout_ignores_keepalive_pings() {
        use futures_util::stream;

        let started = tokio::time::Instant::now();
        let pings = stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Some((Ok(StreamEvent::Ping), ()))
        });
        let provider_stream: ProviderStream = Box::pin(pings);

        let result = consume_with_idle(provider_stream, None, Duration::from_mins(1)).await;

        let Err((err, state)) = result else {
            panic!("a ping-only stream must fail");
        };
        assert_stall_error(&err);
        assert!(started.elapsed() >= Duration::from_mins(1));
        assert!(started.elapsed() < Duration::from_secs(61));
        assert!(can_transparently_retry_stream(&state));
    }

    /// A healthy stream that keeps producing content runs far past the limit:
    /// the limit bounds silence between progress, not total duration.
    #[tokio::test(start_paused = true)]
    async fn stream_idle_timeout_allows_long_streams_that_keep_progressing() {
        use futures_util::stream;

        let started = tokio::time::Instant::now();
        let deltas = stream::unfold(0u32, |n| async move {
            if n == 40 {
                return None;
            }
            tokio::time::sleep(Duration::from_secs(50)).await;
            let event = if n == 0 {
                StreamEvent::ContentBlockStart {
                    index: 0,
                    block_type: ContentBlockType::Text,
                    id: None,
                    name: None,
                    data: None,
                    id_origin: None,
                }
            } else {
                StreamEvent::TextDelta {
                    index: 0,
                    text: format!("part {n}\n"),
                }
            };
            Some((Ok(event), n + 1))
        });
        let provider_stream: ProviderStream = Box::pin(deltas);

        let result = consume_with_idle(provider_stream, None, Duration::from_mins(1)).await;

        assert!(result.is_ok(), "healthy stream must not time out");
        assert!(started.elapsed() >= Duration::from_secs(2000));
    }

    /// Cancelling during a stall ends the turn as interrupted right away, not
    /// as a stall timeout after the limit.
    #[tokio::test(start_paused = true)]
    async fn stream_idle_timeout_yields_to_cancellation() {
        use futures_util::stream;

        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            trigger.cancel();
        });
        let started = tokio::time::Instant::now();
        let provider_stream: ProviderStream =
            Box::pin(stream::iter(reasoning_events()).chain(stream::pending()));

        let result =
            consume_with_idle(provider_stream, Some(&cancel), Duration::from_mins(5)).await;

        let Err((err, _)) = result else {
            panic!("cancelled stream must fail");
        };
        assert!(matches!(err, TurnError::Interrupted { .. }), "got {err:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    struct NeverOpens;

    impl crate::providers::StreamingProvider for NeverOpens {
        fn stream_messages<'a>(
            &'a self,
            _messages: &'a [ChatMessage],
            _tools: &'a [ToolDefinition],
            _system: Option<&'a str>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<ProviderStream>> + Send + 'a>,
        > {
            Box::pin(std::future::pending())
        }
    }

    /// A request that never opens its stream (no response headers) fails with
    /// the same retryable stall error instead of waiting forever.
    #[tokio::test(start_paused = true)]
    async fn stream_idle_timeout_bounds_a_request_that_never_opens() {
        let started = tokio::time::Instant::now();
        let Err(err) = request_stream(
            &NeverOpens,
            &[],
            &[],
            None,
            None,
            Some(Duration::from_mins(5)),
        )
        .await
        else {
            panic!("a request that never opens must fail");
        };
        assert_stall_error(&err);
        assert!(started.elapsed() >= Duration::from_mins(5));
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

        let result = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;
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
                ..
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

        let result = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;
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

        let _ = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;

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

        let _ = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;

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

        let _ = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;

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

        let _ = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;

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

        let _ = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;

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

        let result = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;
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

        let result = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;
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

        let result = consume_stream(
            provider_stream,
            &[],
            &sender,
            Some(&cancel),
            "",
            "",
            std::time::Instant::now(),
        )
        .await;
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
    /// commits its own usage. End-to-end, the channel observes one token-bearing
    /// `UsageUpdate` plus the successful attempt's timing-only terminal update;
    /// the discarded attempt leaves no leak.
    #[allow(clippy::too_many_lines)]
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
        let r1 = consume_stream(s1, &[], &sender, None, "", "", std::time::Instant::now()).await;
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
        let r2 = consume_stream(s2, &[], &sender, None, "", "", std::time::Instant::now()).await;
        assert!(r2.is_ok(), "attempt 2 should succeed");

        // Drain rx and pin the strict ordering: attempt 2's token-bearing usage
        // arrives before visible output, then EOF emits one timing-only update.
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        let usage_positions: Vec<_> = events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match &**e {
                AgentEvent::UsageUpdate {
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                    duration_ms,
                    started_at,
                    completed_at,
                    ..
                } => Some((
                    i,
                    *input_tokens,
                    *output_tokens,
                    *cache_read_input_tokens,
                    *cache_creation_input_tokens,
                    *duration_ms,
                    started_at.clone(),
                    completed_at.clone(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(usage_positions.len(), 2, "got {usage_positions:?}");
        let token_updates: Vec<_> = usage_positions
            .iter()
            .filter(|(_, input, output, cache_read, cache_write, ..)| {
                input + output + cache_read + cache_write > 0
            })
            .collect();
        assert_eq!(
            token_updates.len(),
            1,
            "discarded attempt must not leak token usage: {usage_positions:?}"
        );
        let (usage_idx, usage_input, ..) = token_updates[0];
        assert_eq!(*usage_input, 10, "UsageUpdate must carry attempt 2's input");
        let delta_idx = events
            .iter()
            .position(|e| matches!(&**e, AgentEvent::AssistantDelta { text } if text == "ok"))
            .expect("attempt 2's AssistantDelta must reach the channel");
        assert!(
            *usage_idx < delta_idx,
            "UsageUpdate (idx {usage_idx}) must precede AssistantDelta (idx {delta_idx})"
        );
        let timing = usage_positions
            .iter()
            .find(
                |(_, input, output, cache_read, cache_write, duration, ..)| {
                    input + output + cache_read + cache_write == 0 && duration.is_some()
                },
            )
            .expect("successful attempt must emit terminal timing");
        assert!(timing.0 > delta_idx);
        assert!(timing.6.is_some() && timing.7.is_some());
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

        let result = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;
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

        let result = consume_stream(
            provider_stream,
            &[],
            &sender,
            None,
            "",
            "",
            std::time::Instant::now(),
        )
        .await;
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
            provider: "gemini".to_string(),
            account: None,
            thinking_level: ThinkingLevel::Off,
            client: Box::new(GeminiClient::new(GeminiConfig {
                api_key: "x".to_string(),
                base_url: "https://example.invalid".to_string(),
                model: "gemini-3-pro-preview".to_string(),
                max_output_tokens: None,
                thinking_config: None,
                extra_headers: reqwest::header::HeaderMap::new(),
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
        process_tool_turn(
            &mut messages,
            &mut turn,
            &setup,
            &sender,
            None,
            prior_count,
            None,
            None,
        )
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

    /// A steer pending at the end of a tool round is added after the tool
    /// results, reported, and carried by the round's checkpoint, so the next
    /// model request reads it. A closed source adds nothing.
    #[tokio::test]
    async fn pending_steers_join_the_turn_after_the_tool_round() {
        use std::collections::HashSet;

        use crate::core::steer::{SOURCE_USER, SteerInbox, SteerItem, SteerSource};
        use crate::tools::{ToolContext, ToolRegistry};

        let (tx, mut rx) = create_event_channel();
        let sender = EventSender::new(tx);
        let tool_turn = || {
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
            turn
        };
        let setup = RunTurnSetup {
            model: "gemini-3-pro-preview".to_string(),
            provider: "gemini".to_string(),
            account: None,
            thinking_level: ThinkingLevel::Off,
            client: Box::new(GeminiClient::new(GeminiConfig {
                api_key: "x".to_string(),
                base_url: "https://example.invalid".to_string(),
                model: "gemini-3-pro-preview".to_string(),
                max_output_tokens: None,
                thinking_config: None,
                extra_headers: reqwest::header::HeaderMap::new(),
            })),
            tools: Vec::new(),
            enabled_tools: HashSet::new(),
            tool_ctx: ToolContext::new(std::path::PathBuf::from("."), None),
            tool_registry: ToolRegistry::builtins(),
        };
        let inbox = SteerInbox::new();
        inbox
            .try_push(SteerItem {
                id: "s1".to_string(),
                source: SOURCE_USER.to_string(),
                text: "use the other file".to_string(),
            })
            .unwrap();
        let source = SteerSource::Inbox(inbox.clone());

        let mut messages = vec![ChatMessage::user("first turn")];
        let stats = process_tool_turn(
            &mut messages,
            &mut tool_turn(),
            &setup,
            &sender,
            None,
            1,
            None,
            Some(&source),
        )
        .await
        .unwrap();
        assert_eq!(stats.steered, 1);
        let n = messages.len();
        assert!(messages[n - 2].has_tool_results(), "results come first");
        assert!(messages[n - 1].is_steer());
        assert_eq!(zdx_types::current_turn_start(&messages), 1, "same turn");

        let mut injected = false;
        let mut checkpoint_has_steer = false;
        while let Ok(event) = rx.try_recv() {
            match &*event {
                AgentEvent::UserMessageInjected { id, text, .. } => {
                    injected = id == "s1" && text == "use the other file";
                }
                AgentEvent::TurnCheckpoint { messages, .. } => {
                    checkpoint_has_steer = messages.last().is_some_and(ChatMessage::is_steer);
                }
                _ => {}
            }
        }
        assert!(injected && checkpoint_has_steer);

        inbox
            .try_push(SteerItem {
                id: "s2".to_string(),
                source: SOURCE_USER.to_string(),
                text: "late".to_string(),
            })
            .unwrap();
        source.close();
        let stats = process_tool_turn(
            &mut messages,
            &mut tool_turn(),
            &setup,
            &sender,
            None,
            1,
            None,
            Some(&source),
        )
        .await
        .unwrap();
        assert_eq!(stats.steered, 0, "a closed source adds nothing");
        assert_eq!(inbox.drain_unread().len(), 1, "left for the owner");
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

    /// Two turns of different threads run tools at the same time in one
    /// process. Each turn's children and `$VAR` paths must see that turn's
    /// runtime environment, never the other turn's or the process's.
    #[tokio::test]
    async fn concurrent_turns_keep_their_own_runtime_env() {
        let zdx_home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let config = Config::default();
        let options = bash_policy_options(root.path(), &["bash", "write", "read"], false, None);
        let enabled: HashSet<String> = ["Bash", "Write", "Read"]
            .into_iter()
            .map(String::from)
            .collect();

        let run_turn = |thread: &'static str| {
            let ctx = turn_tool_context(&config, &options, Some(thread), zdx_home.path());
            let enabled = enabled.clone();
            async move {
                let echo = serde_json::json!({
                    "command": "sleep 0.3; printf '%s|%s|%s|%s' \"$ZDX_THREAD_ID\" \"$ZDX_ARTIFACT_DIR\" \"$ZDX_HOME\" \"$ZDX_MEMORY_ROOT\"",
                    "description": "test command",
                });
                let write = serde_json::json!({ "file_path": "$ZDX_ARTIFACT_DIR/out.txt", "content": thread });
                let read = serde_json::json!({ "file_path": "${ZDX_ARTIFACT_DIR}/out.txt" });
                let (bash, write, read) = tokio::join!(
                    crate::tools::execute_tool("bash", "t1", &echo, &ctx, &enabled),
                    crate::tools::execute_tool("write", "t2", &write, &ctx, &enabled),
                    async {
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        crate::tools::execute_tool("read", "t3", &read, &ctx, &enabled).await
                    },
                );
                (bash.0, write.0, read.0)
            }
        };

        let ((bash_a, write_a, read_a), (bash_b, write_b, read_b)) =
            tokio::join!(run_turn("thread-a"), run_turn("thread-b"));

        let memory_root = config
            .memory
            .effective_root_path_with_zdx_home(zdx_home.path())
            .display()
            .to_string();
        for (thread, bash, write, read) in [
            ("thread-a", bash_a, write_a, read_a),
            ("thread-b", bash_b, write_b, read_b),
        ] {
            let artifact_dir = zdx_home.path().join("artifacts/threads").join(thread);
            let data = bash.data().expect("bash succeeded");
            assert_eq!(
                data["stdout"].as_str().unwrap(),
                format!(
                    "{thread}|{}|{}|{memory_root}",
                    artifact_dir.display(),
                    zdx_home.path().display()
                )
            );
            assert!(write.is_ok(), "{write:?}");
            assert_eq!(
                std::fs::read_to_string(artifact_dir.join("out.txt")).unwrap(),
                thread
            );
            let read = read.data().expect("read succeeded");
            assert_eq!(read["content"].as_str().unwrap(), thread);
        }
    }
}
