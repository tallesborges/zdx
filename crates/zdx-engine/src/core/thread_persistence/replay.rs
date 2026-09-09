use anyhow::Result;
use serde_json::Value;

use super::event::{ThreadEvent, Usage, chrono_timestamp};
use super::storage::load_thread_events;

/// Loads thread events and converts them to `ChatMessages` for API use.
///
/// Reconstructs the full thread including tool use/result pairs.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn load_thread_as_messages(id: &str) -> Result<Vec<crate::providers::ChatMessage>> {
    let events = load_thread_events(id)?;
    Ok(thread_events_to_messages(events))
}

/// Returns the runtime-context change key attached to the most recent user
/// message that carries one in a thread's persisted events. `None` when no
/// context has been attached yet (e.g. legacy threads predating the field).
///
/// # Errors
/// Returns an error if the operation fails.
pub fn last_attached_context_key(id: &str) -> Result<Option<String>> {
    let events = load_thread_events(id)?;
    Ok(last_attached_context_key_from_events(&events))
}

/// Scans persisted events for the most recent user message carrying a
/// runtime-context change key. Used for the attach-only-on-meaningful-change
/// dedup rule.
pub fn last_attached_context_key_from_events(events: &[ThreadEvent]) -> Option<String> {
    events.iter().rev().find_map(|event| match event {
        ThreadEvent::Message {
            role,
            context,
            context_key,
            ..
        } if role == "user" && context.is_some() && context_key.is_some() => context_key.clone(),
        _ => None,
    })
}

/// Scans in-memory chat messages for the most recent user message carrying a
/// runtime-context change key. Equivalent to
/// [`last_attached_context_key_from_events`] over replayed messages; used by
/// entry points that already hold messages.
pub fn last_attached_context_key_from_messages(
    messages: &[crate::providers::ChatMessage],
) -> Option<String> {
    messages.iter().rev().find_map(|message| {
        if message.role == "user" && message.context.is_some() && message.context_key.is_some() {
            message.context_key.clone()
        } else {
            None
        }
    })
}

/// Converts chat messages back into thread events for replay/fork bootstrapping.
pub fn messages_to_events(messages: &[crate::providers::ChatMessage]) -> Vec<ThreadEvent> {
    let mut events = Vec::new();
    for message in messages {
        emit_message_events(message, &mut events);
    }
    events
}

/// Walks a single `ChatMessage` and appends the per-block `ThreadEvent`s it
/// represents, preserving original block order and per-part replay metadata.
///
/// Shared by `UsagePersistor::flush_messages` (the live write path) and the
/// public `messages_to_events` converter so any caller round-tripping
/// `ChatMessage`s produces the same on-disk shape. Per-part Gemini
/// `thoughtSignature`s and tool-use `id_origin` data must survive both
/// paths or implicit-cache replay breaks.
pub(crate) fn emit_message_events(
    msg: &crate::providers::ChatMessage,
    events: &mut Vec<ThreadEvent>,
) {
    use crate::providers::{ChatContentBlock, MessageContent};

    match &msg.content {
        MessageContent::Text(text) => {
            events.push(ThreadEvent::Message {
                role: msg.role.clone(),
                text: text.clone(),
                phase: msg.phase.clone(),
                context: msg.context.clone(),
                context_key: msg.context_key.clone(),
                replay: None,
                ts: chrono_timestamp(),
            });
        }
        MessageContent::Blocks(blocks) => {
            // A user message's runtime-context block and change key are
            // persisted on the first text event so replay reconstructs the
            // projection once.
            let mut context = if msg.role == "user" {
                (msg.context.clone(), msg.context_key.clone())
            } else {
                (None, None)
            };
            let mut wrote_user_text = false;
            for block in blocks {
                match block {
                    ChatContentBlock::Text { text, replay } => {
                        wrote_user_text = true;
                        events.push(ThreadEvent::Message {
                            role: msg.role.clone(),
                            text: text.clone(),
                            phase: msg.phase.clone(),
                            context: context.0.take(),
                            context_key: context.1.take(),
                            replay: replay.clone(),
                            ts: chrono_timestamp(),
                        });
                    }
                    ChatContentBlock::ToolUse {
                        id,
                        name,
                        input,
                        id_origin,
                        replay,
                    } => {
                        events.push(ThreadEvent::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                            id_origin: *id_origin,
                            replay: replay.clone(),
                            ts: chrono_timestamp(),
                        });
                    }
                    ChatContentBlock::Reasoning(rb) => {
                        events.push(ThreadEvent::Reasoning {
                            text: rb.text.clone(),
                            replay: rb.replay.clone(),
                            ts: chrono_timestamp(),
                        });
                    }
                    ChatContentBlock::ToolResult(tr) => {
                        let output = tr
                            .content
                            .as_text()
                            .and_then(|s| serde_json::from_str::<Value>(s).ok())
                            .unwrap_or(Value::Null);
                        events.push(ThreadEvent::ToolResult {
                            tool_use_id: tr.tool_use_id.clone(),
                            output,
                            ok: !tr.is_error,
                            duration_ms: None,
                            ts: chrono_timestamp(),
                        });
                    }
                    // Image content is not persisted in thread files.
                    // Tool-result image bytes are dropped on the way through
                    // `ToolResult.content.as_text()` already; user-side image
                    // attachments are not part of the current persistence
                    // schema.
                    ChatContentBlock::Image { .. } => {}
                }
            }
            // A user message that carries runtime-context metadata but no text
            // block (e.g. an image-only TUI send) would otherwise drop the
            // snapshot. Persist it on a synthetic empty-text event so the
            // context block/key and its text projection survive replay. This
            // preserves the metadata, not image bytes — image replay fidelity
            // is not provided (live is `Blocks([context, image…])`, replay is
            // an empty-text message with the projected context).
            if msg.role == "user"
                && !wrote_user_text
                && (msg.context.is_some() || msg.context_key.is_some())
            {
                events.push(ThreadEvent::Message {
                    role: "user".to_string(),
                    text: String::new(),
                    phase: msg.phase.clone(),
                    context: msg.context.clone(),
                    context_key: msg.context_key.clone(),
                    replay: None,
                    ts: chrono_timestamp(),
                });
            }
        }
    }
}

/// Converts thread events to chat messages for API replay.
pub fn thread_events_to_messages(events: Vec<ThreadEvent>) -> Vec<crate::providers::ChatMessage> {
    let mut replay = MessageReplay::new();
    for event in events {
        replay.handle_event(event);
    }
    replay.finalize()
}

struct MessageReplay {
    messages: Vec<crate::providers::ChatMessage>,
    /// Ordered buffer of assistant-side blocks accumulated since the last
    /// flush. Preserves the exact arrival order of `Reasoning`, `Text`, and
    /// `ToolUse` events on disk — required so Gemini per-part replay
    /// metadata stays attached to the right block.
    pending_assistant_blocks: Vec<crate::providers::ChatContentBlock>,
    /// Phase to attach when the pending assistant blocks are flushed
    /// (e.g. `Some("commentary")` for an interrupted turn). Set when the
    /// first phase-bearing assistant `Message` event of the batch arrives,
    /// or explicitly by `Interrupted` handling.
    pending_assistant_phase: Option<String>,
    pending_tool_results: Vec<crate::tools::ToolResult>,
    open_tool_uses: Vec<String>,
}

impl MessageReplay {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
            pending_assistant_blocks: Vec::new(),
            pending_assistant_phase: None,
            pending_tool_results: Vec::new(),
            open_tool_uses: Vec::new(),
        }
    }

    fn handle_event(&mut self, event: ThreadEvent) {
        match event {
            // Non-replay events: meta, usage, and informational notices
            // (the latter are UI-only and MUST NOT be sent back to the
            // provider as part of the conversation).
            ThreadEvent::Meta { .. } | ThreadEvent::Usage { .. } | ThreadEvent::Notice { .. } => {}
            ThreadEvent::Message {
                role,
                text,
                phase,
                context,
                context_key,
                replay,
                ..
            } => self.handle_message(role, text, phase, context, context_key, replay),
            ThreadEvent::Reasoning { text, replay, .. } => {
                self.flush_tool_results();
                self.pending_assistant_blocks
                    .push(crate::providers::ChatContentBlock::Reasoning(
                        crate::providers::ReasoningBlock { text, replay },
                    ));
            }
            ThreadEvent::ToolUse {
                id,
                name,
                input,
                id_origin,
                replay,
                ..
            } => {
                self.flush_tool_results();
                self.open_tool_uses.push(id.clone());
                self.pending_assistant_blocks
                    .push(crate::providers::ChatContentBlock::ToolUse {
                        id,
                        name,
                        input,
                        id_origin,
                        replay,
                    });
            }
            ThreadEvent::ToolResult {
                tool_use_id,
                output,
                ok,
                ..
            } => self.handle_tool_result(tool_use_id, &output, ok),
            ThreadEvent::Interrupted { .. } => self.handle_interrupted(),
        }
    }

    fn handle_message(
        &mut self,
        role: String,
        text: String,
        phase: Option<String>,
        context: Option<String>,
        context_key: Option<String>,
        replay: Option<crate::providers::ReplayToken>,
    ) {
        use crate::providers::{ChatContentBlock, ChatMessage, MessageContent};

        if role == "assistant" {
            // Append to ordered pending blocks; capture the message-level
            // phase if any. Subsequent flushes will attach this phase to the
            // assembled assistant message.
            self.flush_tool_results();
            if self.pending_assistant_phase.is_none() {
                self.pending_assistant_phase = phase;
            }
            self.pending_assistant_blocks
                .push(ChatContentBlock::Text { text, replay });
            return;
        }

        // Non-assistant Message: flush any in-flight assistant blocks first,
        // cancel orphaned tool_uses, then push the user/system message.
        self.flush_pending_assistant_blocks();
        self.cancel_open_tool_uses();
        self.messages.push(ChatMessage {
            role,
            phase,
            context,
            context_key,
            content: MessageContent::Text(text),
        });
    }

    fn handle_tool_result(&mut self, tool_use_id: String, output: &Value, ok: bool) {
        self.open_tool_uses.retain(|id| id != &tool_use_id);
        self.flush_pending_assistant_blocks();
        self.pending_tool_results.push(crate::tools::ToolResult {
            tool_use_id,
            content: crate::tools::ToolResultContent::Text(
                serde_json::to_string(output).unwrap_or_default(),
            ),
            is_error: !ok,
        });
    }

    fn handle_interrupted(&mut self) {
        self.flush_tool_results();
        // Mark the in-flight assistant batch as commentary: partial text
        // arrives via earlier `Message` events, and the request builder
        // tags an interrupted turn's text as commentary on replay.
        self.pending_assistant_phase = Some("commentary".to_string());
        self.flush_pending_assistant_blocks();

        for tool_use_id in std::mem::take(&mut self.open_tool_uses) {
            self.pending_tool_results
                .push(cancelled_tool_result(tool_use_id));
        }
        self.flush_tool_results();
    }

    fn flush_tool_results(&mut self) {
        if !self.pending_tool_results.is_empty() {
            self.messages
                .push(crate::providers::ChatMessage::tool_results(std::mem::take(
                    &mut self.pending_tool_results,
                )));
        }
    }

    fn cancel_open_tool_uses(&mut self) {
        for tool_use_id in std::mem::take(&mut self.open_tool_uses) {
            self.pending_tool_results
                .push(cancelled_tool_result(tool_use_id));
        }
        self.flush_tool_results();
    }

    fn flush_pending_assistant_blocks(&mut self) {
        if self.pending_assistant_blocks.is_empty() {
            self.pending_assistant_phase = None;
            return;
        }
        let blocks = std::mem::take(&mut self.pending_assistant_blocks);
        let phase = self.pending_assistant_phase.take();
        self.messages.push(crate::providers::ChatMessage {
            role: "assistant".to_string(),
            phase,
            context: None,
            context_key: None,
            content: crate::providers::MessageContent::Blocks(blocks),
        });
        self.flush_tool_results();
    }

    fn finalize(mut self) -> Vec<crate::providers::ChatMessage> {
        self.flush_pending_assistant_blocks();
        self.cancel_open_tool_uses();
        self.messages
    }
}

fn cancelled_tool_result(tool_use_id: String) -> crate::tools::ToolResult {
    let output = serde_json::json!({
        "ok": false,
        "error": {
            "code": "cancelled",
            "message": "Tool call was cancelled by user."
        }
    });
    crate::tools::ToolResult {
        tool_use_id,
        content: crate::tools::ToolResultContent::Text(
            serde_json::to_string(&output).unwrap_or_default(),
        ),
        is_error: true,
    }
}

/// Extracts `(cumulative, latest)` usage from thread events for thread restore.
///
/// Cumulative sums all `Usage` events (for cost). `latest` is the most recent
/// *request* for context-% display, which must be reconstructed: a single
/// request persists as one context-bearing event followed by output-only tail
/// fragments `(0, output, 0, 0)`, so the literal last event has zero
/// `context_input()`. Mirrors the live `ThreadUsage::add` logic.
pub fn extract_usage_from_thread_events(events: &[ThreadEvent]) -> (Usage, Usage) {
    let mut cumulative = Usage::default();
    let mut latest = Usage::default();

    for event in events {
        if let ThreadEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            ..
        } = event
        {
            let usage = Usage::new(
                *input_tokens,
                *output_tokens,
                *cache_read_tokens,
                *cache_write_tokens,
            );

            cumulative += usage;

            // Context-bearing event starts a new request; output-only tails fold in.
            if usage.input > 0 || usage.cache_read > 0 || usage.cache_write > 0 {
                latest = usage;
            } else {
                latest.output += usage.output;
            }
        }
    }

    (cumulative, latest)
}
