use serde::{Deserialize, Serialize};
use serde_json::Value;

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// Token usage data for a single API request.
///
/// Used for both persistence (in thread files) and runtime tracking.
/// Supports event-sourcing: each request saves its own Usage, and cumulative
/// totals are derived by summing all events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens (non-cached) for this request
    pub input: u64,
    /// Output tokens for this request
    pub output: u64,
    /// Tokens read from cache for this request
    pub cache_read: u64,
    /// Tokens written to cache for this request
    pub cache_write: u64,
}

impl Usage {
    /// Creates a new Usage with all fields set.
    pub fn new(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write,
        }
    }

    /// Total tokens for this request (for context window calculation).
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// Context tokens (input side) for context window percentage.
    pub fn context_input(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    /// Adds another Usage to this one (for accumulation).
    pub fn add(&mut self, other: &Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }

    /// Returns a new Usage that is the sum of self and other.
    #[must_use]
    pub fn plus(&self, other: &Usage) -> Usage {
        Usage {
            input: self.input + other.input,
            output: self.output + other.output,
            cache_read: self.cache_read + other.cache_read,
            cache_write: self.cache_write + other.cache_write,
        }
    }
}

impl std::ops::Add for Usage {
    type Output = Usage;

    fn add(self, other: Usage) -> Usage {
        self.plus(&other)
    }
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, other: Usage) {
        self.add(&other);
    }
}

/// Current schema version for new threads.
pub const SCHEMA_VERSION: u32 = 1;

/// A thread event (polymorphic, tag-based).
///
/// This enum represents all event types that can be persisted in a thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreadEvent {
    /// Meta event: first line of a v1+ thread file.
    Meta {
        schema_version: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root_path: Option<String>,
        /// The ID of the parent thread this was handed off from (if any).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handoff_from: Option<String>,
        /// Origin kind for threads spawned by another agent run (e.g.
        /// `"subagent"`, `"helper:title"`). `None` for top-level user threads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin_kind: Option<String>,
        /// Parent thread id that spawned this run (subagent/helper). `None`
        /// for top-level user threads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_thread_id: Option<String>,
        /// Named subagent (e.g. `"explorer"`) when `origin_kind == "subagent"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_name: Option<String>,
        /// Model override for this thread (overrides config.model).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_override: Option<String>,
        /// Thinking override for this thread (overrides `config.thinking_level`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking_override: Option<crate::config::ThinkingLevel>,
        /// Whether the next qualifying user message should generate the topic title.
        #[serde(default, skip_serializing_if = "is_false")]
        pending_topic_title: bool,
        ts: String,
    },

    /// Message event: user or assistant text.
    Message {
        role: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        /// Provider-specific replay metadata (e.g. Gemini per-text-part
        /// `thoughtSignature`). Persisted so the request builder can replay
        /// the signature exactly on the next turn. Defaults to `None` for
        /// older transcripts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay: Option<crate::providers::ReplayToken>,
        ts: String,
    },

    /// Tool use event: model requested a tool call.
    ToolUse {
        id: String,
        name: String,
        input: Value,
        /// Whether `id` was emitted by the provider (`Real`) or synthesized
        /// locally because the provider omitted one (`Synthesized`). Used by
        /// the Gemini request builder to decide whether to replay the id on
        /// the wire. Defaults to `Synthesized` for migration safety.
        #[serde(default)]
        id_origin: zdx_types::IdOrigin,
        /// Provider-specific replay metadata (e.g. Gemini per-tool-call
        /// `thoughtSignature`). Defaults to `None` for older transcripts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay: Option<crate::providers::ReplayToken>,
        ts: String,
    },

    /// Tool result event: output from tool execution.
    ToolResult {
        tool_use_id: String,
        output: Value,
        ok: bool,
        ts: String,
    },

    /// Interrupted event: thread was interrupted by user.
    Interrupted {
        #[serde(default = "default_interrupted_role")]
        role: String,
        #[serde(default = "default_interrupted_text")]
        text: String,
        ts: String,
    },

    /// Reasoning event: provider-agnostic reasoning block with replay data.
    Reasoning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay: Option<crate::providers::ReplayToken>,
        ts: String,
    },

    /// Usage event: token usage snapshot after a turn.
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
        /// Model id that produced this usage. `None` on older transcripts and
        /// on usage recorded before per-request attribution existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Provider id that served the request (e.g. `anthropic`, `claude-cli`,
        /// or a custom provider name). `None` on older transcripts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// Wall-clock request duration in milliseconds. `Some` only on the
        /// terminal usage event of a successful request. `None` on older
        /// transcripts and interim/failed usage.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// Time-to-first-token in milliseconds. `Some` only on the terminal
        /// usage event when content arrived. `None` on older transcripts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttft_ms: Option<u64>,
        ts: String,
    },

    /// Informational notice from the model or runtime
    /// (e.g. `refusal`, `model_context_window_exceeded`).
    ///
    /// Persisted so the UI can render it on thread reload, but never
    /// rehydrated as a chat message — it is purely informational and
    /// MUST NOT be sent back to providers as part of the conversation.
    Notice {
        kind: zdx_types::NoticeKind,
        message: String,
        ts: String,
    },
}

impl ThreadEvent {
    /// Creates a new meta event with an optional root path.
    pub fn meta_with_root(root_path: Option<String>) -> Self {
        Self::Meta {
            schema_version: SCHEMA_VERSION,
            title: None,
            root_path,
            handoff_from: None,
            origin_kind: None,
            parent_thread_id: None,
            subagent_name: None,
            model_override: None,
            thinking_override: None,
            pending_topic_title: false,
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new meta event with root path and handoff source.
    pub fn meta_with_root_and_source(
        root_path: Option<String>,
        handoff_from: Option<String>,
    ) -> Self {
        Self::Meta {
            schema_version: SCHEMA_VERSION,
            title: None,
            root_path,
            handoff_from,
            origin_kind: None,
            parent_thread_id: None,
            subagent_name: None,
            model_override: None,
            thinking_override: None,
            pending_topic_title: false,
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new meta event carrying subagent/helper lineage.
    pub fn meta_with_lineage(
        root_path: Option<String>,
        handoff_from: Option<String>,
        origin_kind: Option<String>,
        parent_thread_id: Option<String>,
        subagent_name: Option<String>,
    ) -> Self {
        Self::Meta {
            schema_version: SCHEMA_VERSION,
            title: None,
            root_path,
            handoff_from,
            origin_kind,
            parent_thread_id,
            subagent_name,
            model_override: None,
            thinking_override: None,
            pending_topic_title: false,
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new user message event.
    pub fn user_message(text: impl Into<String>) -> Self {
        Self::Message {
            role: "user".to_string(),
            text: text.into(),
            phase: None,
            replay: None,
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new assistant message event.
    pub fn assistant_message(text: impl Into<String>) -> Self {
        Self::assistant_message_with_phase(text, None)
    }

    /// Creates a new assistant message event with optional phase.
    pub fn assistant_message_with_phase(text: impl Into<String>, phase: Option<String>) -> Self {
        Self::Message {
            role: "assistant".to_string(),
            text: text.into(),
            phase,
            replay: None,
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new tool use event with synthesized id and no replay metadata.
    pub fn tool_use(id: impl Into<String>, name: impl Into<String>, input: Value) -> Self {
        Self::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
            id_origin: zdx_types::IdOrigin::Synthesized,
            replay: None,
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new tool result event.
    pub fn tool_result(tool_use_id: impl Into<String>, output: Value, ok: bool) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            output,
            ok,
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new interrupted event.
    pub fn interrupted() -> Self {
        Self::Interrupted {
            role: default_interrupted_role(),
            text: default_interrupted_text(),
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new reasoning event.
    pub fn reasoning(text: Option<String>, replay: Option<crate::providers::ReplayToken>) -> Self {
        Self::Reasoning {
            text,
            replay,
            ts: chrono_timestamp(),
        }
    }

    /// Creates a new usage event with optional model/provider attribution and
    /// per-request latency (present only on a request's terminal usage event).
    pub fn usage(
        usage: Usage,
        model: Option<String>,
        provider: Option<String>,
        duration_ms: Option<u64>,
        ttft_ms: Option<u64>,
    ) -> Self {
        Self::Usage {
            input_tokens: usage.input,
            output_tokens: usage.output,
            cache_read_tokens: usage.cache_read,
            cache_write_tokens: usage.cache_write,
            model,
            provider,
            duration_ms,
            ttft_ms,
            ts: chrono_timestamp(),
        }
    }

    /// Converts an `AgentEvent` to a `ThreadEvent` if applicable.
    ///
    /// Streaming `AgentEvent`s (text/reasoning/tool-input deltas and their
    /// `*Completed` siblings) are intentionally not persisted here.
    /// `UsagePersistor` consumes `TurnCheckpoint` / `TurnFinished` and
    /// writes ordered batches of `ThreadEvent`s by walking the snapshot
    /// `messages: Vec<ChatMessage>` from `prior_message_count..` (see
    /// `flush_messages`). This guarantees on-disk block order matches what
    /// the provider streamed, which Gemini's implicit prompt cache
    /// depends on.
    ///
    /// This function still produces:
    /// - `ThreadEvent::Interrupted` as an interruption marker. Partial
    ///   assistant blocks for the interrupted turn are flushed by
    ///   `flush_messages` as a `commentary`-phased `Message` event before
    ///   this marker, so consumers can detect interruption without the
    ///   marker itself carrying any payload.
    /// - `ThreadEvent::Notice` for non-fatal informational notices.
    pub fn from_agent(event: &crate::core::events::AgentEvent) -> Option<Self> {
        use crate::core::events::AgentEvent;

        match event {
            AgentEvent::TurnFinished {
                status: crate::core::events::TurnStatus::Interrupted,
                ..
            } => Some(Self::interrupted()),
            // Persist informational notices (refusal, context window
            // exceeded) as a non-replayable thread event so they survive
            // reload but are NEVER re-sent to the provider as part of
            // the conversation history.
            AgentEvent::Notice { kind, message, .. } => Some(Self::Notice {
                kind: kind.clone(),
                message: message.clone(),
                ts: chrono_timestamp(),
            }),
            // Streaming events are consumed by the TUI directly; persistence
            // batches them via `flush_messages` on `TurnCheckpoint` /
            // `TurnFinished`.
            _ => None,
        }
    }
}

fn default_interrupted_role() -> String {
    "system".to_string()
}

fn default_interrupted_text() -> String {
    "Interrupted".to_string()
}

/// Returns an RFC3339 UTC timestamp string.
pub(crate) fn chrono_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub(crate) fn normalize_title(title: impl Into<String>) -> Option<String> {
    let trimmed = title.into().trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}
