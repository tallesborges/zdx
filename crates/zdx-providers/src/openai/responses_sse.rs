//! SSE parsing for OpenAI-compatible Responses streaming.

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;

use eventsource_stream::{EventStream, Eventsource};
use futures_util::Stream;
use serde_json::Value;

use crate::{
    ContentBlockType, ProviderError, ProviderErrorKind, ProviderResult, StreamEvent, Usage,
    error_message_from_payload, map_event_stream_error,
};

/// Extension trait for extracting strings from JSON values.
trait JsonExt {
    /// Get a string field, returning empty string if missing or not a string.
    fn get_str(&self, key: &str) -> &str;
    /// Get a string field as owned String, returning empty string if missing.
    fn get_string(&self, key: &str) -> String;
}

impl JsonExt for Value {
    fn get_str(&self, key: &str) -> &str {
        self.get(key).and_then(|v| v.as_str()).unwrap_or("")
    }

    fn get_string(&self, key: &str) -> String {
        self.get_str(key).to_string()
    }
}

/// Kind of the current non-tool block. Tool calls are tracked separately in
/// `StreamState::tool_calls`, since they may interleave with each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Reasoning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalOutcome {
    Completed,
    Incomplete,
}

/// State for tracking a reasoning item being streamed.
#[derive(Debug, Clone)]
struct ReasoningState {
    index: usize,
    id: String,
    summary: String,
}

/// Per-tool-call argument accumulation. Each registered call owns its own
/// buffer, so parallel or delayed calls cannot splice arguments into each
/// other. Entries are retained for the whole response (never removed on
/// completion) so a duplicate terminal event cannot migrate onto a later call.
#[derive(Debug)]
struct ToolCallState {
    stream_index: usize,
    /// Everything already emitted downstream for this call. Kept in full
    /// rather than as a byte count so a final snapshot can be verified to
    /// extend what was streamed instead of merely being longer than it.
    arguments: String,
    closed: bool,
}

#[derive(Debug)]
struct StreamState {
    next_index: usize,
    current_index: Option<usize>,
    current_kind: Option<BlockKind>,
    saw_tool: bool,
    /// Tracks reasoning item being streamed (for summary replay)
    current_reasoning: Option<ReasoningState>,
    /// Tool calls seen this response, keyed by their block index.
    tool_calls: HashMap<usize, ToolCallState>,
    /// Responses `output_index` → block index.
    tool_by_output: HashMap<u64, usize>,
    /// Responses item id → block index. This is the item id, never `call_id`
    /// and never the composite replay id sent on `ContentBlockStart`.
    tool_by_item_id: HashMap<String, usize>,
}

impl StreamState {
    fn new() -> Self {
        Self {
            next_index: 0,
            current_index: None,
            current_kind: None,
            saw_tool: false,
            current_reasoning: None,
            tool_calls: HashMap::new(),
            tool_by_output: HashMap::new(),
            tool_by_item_id: HashMap::new(),
        }
    }

    /// Registers a tool call opened by `response.output_item.added`, together
    /// with every identifier later events may use to address it.
    ///
    /// # Errors
    /// Returns a parse error if an identifier is already bound to a different
    /// call; overwriting it would silently redirect another call's arguments.
    fn open_tool(
        &mut self,
        stream_index: usize,
        item_id: &str,
        output_index: Option<u64>,
    ) -> ProviderResult<()> {
        if let Some(index) = output_index
            && let Some(previous) = self.tool_by_output.insert(index, stream_index)
            && previous != stream_index
        {
            return Err(tool_identity_error(&format!(
                "output_index {index} was already bound to tool block {previous}"
            )));
        }
        if !item_id.is_empty()
            && let Some(previous) = self
                .tool_by_item_id
                .insert(item_id.to_string(), stream_index)
            && previous != stream_index
        {
            return Err(tool_identity_error(&format!(
                "item id {item_id:?} was already bound to tool block {previous}"
            )));
        }
        self.tool_calls.insert(
            stream_index,
            ToolCallState {
                stream_index,
                arguments: String::new(),
                closed: false,
            },
        );
        Ok(())
    }

    /// Resolves which tool call an argument or terminal event addresses.
    ///
    /// Identity is never inferred from recency: guessing the latest call is
    /// exactly what let one call's arguments land on another. Every supplied
    /// identifier must resolve on its own, and identifiers that both resolve
    /// must agree; a stale or invented alias cannot ride along on a sibling
    /// identifier that happens to be valid. An unlabeled event is accepted
    /// only when a single tool call exists in the whole response, which is
    /// the only case where it is unambiguous.
    ///
    /// # Errors
    /// Returns a parse error when a supplied identifier is unknown, when the
    /// identifiers disagree, or when an unlabeled event arrives while several
    /// calls exist.
    fn resolve_tool(&self, item_id: &str, output_index: Option<u64>) -> ProviderResult<usize> {
        let by_item_id = if item_id.is_empty() {
            None
        } else {
            let slot = self.tool_by_item_id.get(item_id).copied();
            if slot.is_none() {
                return Err(tool_identity_error(&format!(
                    "no tool call registered for item id {item_id:?}"
                )));
            }
            slot
        };
        let by_output = if let Some(index) = output_index {
            let slot = self.tool_by_output.get(&index).copied();
            if slot.is_none() {
                return Err(tool_identity_error(&format!(
                    "no tool call registered for output_index {index}"
                )));
            }
            slot
        } else {
            None
        };

        match (by_item_id, by_output) {
            (Some(from_id), Some(from_index)) if from_id != from_index => {
                Err(tool_identity_error(&format!(
                    "item id {item_id:?} resolves to tool block {from_id} but output_index \
                     {output_index:?} resolves to tool block {from_index}"
                )))
            }
            (Some(slot), _) | (None, Some(slot)) => Ok(slot),
            (None, None) => match self.tool_calls.len() {
                1 => Ok(*self
                    .tool_calls
                    .keys()
                    .next()
                    .expect("map with one entry has a key")),
                other => Err(tool_identity_error(&format!(
                    "unidentified tool argument event with {other} tool calls in the response"
                ))),
            },
        }
    }
}

/// A tool call could not be addressed unambiguously. This is fatal rather
/// than retryable: continuing would hand the model's arguments to the wrong
/// call, or drop them.
fn tool_identity_error(detail: &str) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Parse,
        format!("Responses stream tool-call identity error: {detail}"),
    )
}

/// Reads the `item_id`/`output_index` pair that labels a Responses event.
fn tool_event_labels(value: &Value, item_id_key: &str) -> (String, Option<u64>) {
    (
        value.get_string(item_id_key),
        value.get("output_index").and_then(Value::as_u64),
    )
}

fn extract_function_call_arguments(item: &Value) -> Option<String> {
    if let Some(arguments) = item.get("arguments") {
        return if let Some(text) = arguments.as_str() {
            Some(text.to_string())
        } else if arguments.is_null() {
            None
        } else {
            Some(arguments.to_string())
        };
    }

    item.get("input").and_then(|input| {
        if let Some(text) = input.as_str() {
            Some(text.to_string())
        } else if input.is_null() {
            None
        } else {
            Some(input.to_string())
        }
    })
}

fn usage_from_response(response: &Value) -> Usage {
    let usage = response.get("usage").unwrap_or(&Value::Null);
    let input_details = usage.get("input_tokens_details").unwrap_or(&Value::Null);
    let cache_read_input_tokens = input_details
        .get("cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_creation_input_tokens = input_details
        .get("cache_write_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let input_tokens = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_sub(cache_read_input_tokens)
        .saturating_sub(cache_creation_input_tokens);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Usage {
        input_tokens,
        output_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
    }
}

/// Maps `OpenAI` Responses JSON events to `StreamEvent`s. The payloads are
/// byte-identical over SSE and WebSocket, so both transports feed this mapper.
pub struct ResponsesEventMapper {
    model: String,
    state: StreamState,
    pending: VecDeque<StreamEvent>,
    last_response_id: Option<String>,
    terminal_outcome: Option<TerminalOutcome>,
    requested_service_tier: Option<String>,
}

impl ResponsesEventMapper {
    pub fn new(model: String) -> Self {
        Self {
            model,
            state: StreamState::new(),
            pending: VecDeque::new(),
            last_response_id: None,
            terminal_outcome: None,
            requested_service_tier: None,
        }
    }

    /// Records the `service_tier` asked for, so a silent downgrade is logged.
    #[must_use]
    pub fn with_requested_service_tier(mut self, tier: Option<String>) -> Self {
        self.requested_service_tier = tier;
        self
    }

    /// Parses one JSON event payload and queues the resulting `StreamEvent`(s).
    ///
    /// # Errors
    /// Returns a parse error if the payload is not valid JSON.
    pub fn push_json(&mut self, data: &str) -> ProviderResult<()> {
        let trimmed = data.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        if trimmed == "[DONE]" {
            return if self.terminal_outcome.is_some() {
                Ok(())
            } else {
                Err(ProviderError::transport(
                    "Responses stream ended before a terminal event",
                ))
            };
        }

        let value = serde_json::from_str::<Value>(trimmed).map_err(|err| {
            ProviderError::new(
                ProviderErrorKind::Parse,
                format!("Failed to parse SSE JSON: {err}"),
            )
        })?;
        let event = self.map_event(value)?;
        self.pending.push_back(event);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<StreamEvent> {
        self.pending.pop_front()
    }

    /// Most recent server `response.id` (used for WebSocket continuation).
    pub fn last_response_id(&self) -> Option<&str> {
        self.last_response_id.as_deref()
    }

    /// Logs when the provider served a different tier than the one requested,
    /// so `@fast` can never silently cost more without being faster.
    fn warn_on_service_tier_downgrade(&self, response: &Value) {
        let Some(requested) = self.requested_service_tier.as_deref() else {
            return;
        };
        let granted = response.get_str("service_tier");
        if !granted.is_empty() && granted != requested {
            tracing::warn!(
                model = %self.model,
                requested,
                granted,
                "provider downgraded the requested service tier"
            );
        }
    }

    pub(crate) fn terminal_outcome(&self) -> Option<TerminalOutcome> {
        self.terminal_outcome
    }

    /// Closes a tool call, reconciling its streamed arguments against the
    /// snapshot on the terminal event.
    ///
    /// The snapshot must *extend* what was already streamed. Comparing only
    /// lengths cannot tell "the model sent more" from "the provider sent
    /// something else", and appending the tail of a disagreeing snapshot is
    /// what produced valid JSON followed by a stray suffix. A snapshot that
    /// is not a continuation fails the stream rather than emitting arguments
    /// the model never produced.
    ///
    /// A second terminal for an already-closed call is idempotent only when
    /// it says nothing new: no snapshot, or a snapshot identical to what was
    /// emitted. Anything else is contradictory, including a snapshot that
    /// would extend the call, since input cannot follow its own
    /// `ContentBlockCompleted`.
    ///
    /// # Errors
    /// Returns a parse error when the snapshot contradicts the streamed
    /// prefix, or when a duplicate terminal carries different arguments.
    fn finish_tool_call(
        &mut self,
        slot: usize,
        final_arguments: Option<String>,
    ) -> ProviderResult<StreamEvent> {
        let Some(call) = self.state.tool_calls.get_mut(&slot) else {
            return Err(tool_identity_error(&format!(
                "terminal event for unregistered tool block {slot}"
            )));
        };

        let index = call.stream_index;

        if call.closed {
            return match final_arguments {
                None => Ok(StreamEvent::Ping),
                Some(final_arguments) if final_arguments == call.arguments => Ok(StreamEvent::Ping),
                Some(final_arguments) => Err(ProviderError::new(
                    ProviderErrorKind::Parse,
                    format!(
                        "Responses stream tool-call argument mismatch on block {index}: a \
                         duplicate terminal carries {} bytes that differ from the {} bytes \
                         already completed",
                        final_arguments.len(),
                        call.arguments.len()
                    ),
                )),
            };
        }

        let mut remainder = String::new();

        if let Some(final_arguments) = final_arguments {
            if let Some(suffix) = final_arguments.strip_prefix(call.arguments.as_str()) {
                remainder = suffix.to_string();
                call.arguments = final_arguments;
            } else {
                let streamed_len = call.arguments.len();
                let final_len = final_arguments.len();
                let divergence = call
                    .arguments
                    .as_bytes()
                    .iter()
                    .zip(final_arguments.as_bytes())
                    .position(|(a, b)| a != b)
                    .unwrap_or(final_len.min(streamed_len));
                return Err(ProviderError::new(
                    ProviderErrorKind::Parse,
                    format!(
                        "Responses stream tool-call argument mismatch on block {index}: \
                             final arguments ({final_len} bytes) do not extend the {streamed_len} \
                             bytes already streamed (first difference at byte {divergence})"
                    ),
                ));
            }
        }

        call.closed = true;

        if !remainder.is_empty() {
            self.pending.push_back(StreamEvent::InputJsonDelta {
                index,
                partial_json: remainder,
            });
        }

        Ok(StreamEvent::ContentBlockCompleted {
            index,
            signature: None,
        })
    }

    #[allow(
        clippy::too_many_lines,
        clippy::needless_pass_by_value,
        clippy::unnecessary_wraps
    )]
    fn map_event(&mut self, value: Value) -> ProviderResult<StreamEvent> {
        let event_type = value.get_str("type");

        match event_type {
            "response.output_item.added" => {
                let item = value.get("item").unwrap_or(&Value::Null);
                let item_type = item.get_str("type");
                match item_type {
                    "message" => {
                        let index = self.state.next_index;
                        self.state.next_index += 1;
                        self.state.current_index = Some(index);
                        self.state.current_kind = Some(BlockKind::Text);
                        Ok(StreamEvent::ContentBlockStart {
                            index,
                            block_type: ContentBlockType::Text,
                            id: None,
                            name: None,
                            data: None,
                            id_origin: None,
                        })
                    }
                    "function_call" => {
                        let index = self.state.next_index;
                        self.state.next_index += 1;
                        self.state.saw_tool = true;

                        let call_id = item.get_str("call_id");
                        let id = item.get_str("id");
                        let name = item.get_str("name");
                        let tool_id = if !call_id.is_empty() && !id.is_empty() {
                            format!("{call_id}|{id}")
                        } else {
                            format!("{call_id}{id}")
                        };

                        // A tool call owns its own argument buffer rather than
                        // the shared current-block slot: Responses streams may
                        // interleave parallel calls, and the slot would then
                        // splice one call's arguments onto another's.
                        let output_index = value.get("output_index").and_then(Value::as_u64);
                        let item_id = item.get_string("id");
                        self.state.open_tool(index, &item_id, output_index)?;

                        Ok(StreamEvent::ContentBlockStart {
                            index,
                            block_type: ContentBlockType::ToolUse,
                            id: Some(tool_id),
                            name: Some(name.to_string()),
                            data: None,
                            id_origin: None,
                        })
                    }
                    "reasoning" => {
                        // Initialize reasoning state for streaming
                        let index = self.state.next_index;
                        self.state.next_index += 1;
                        self.state.current_index = Some(index);
                        self.state.current_kind = Some(BlockKind::Reasoning);

                        self.state.current_reasoning = Some(ReasoningState {
                            index,
                            id: item.get_string("id"),
                            summary: String::new(),
                        });

                        // Emit ContentBlockStart for reasoning so agent.rs tracks it
                        Ok(StreamEvent::ContentBlockStart {
                            index,
                            block_type: ContentBlockType::Reasoning,
                            id: None,
                            name: None,
                            data: None,
                            id_origin: None,
                        })
                    }
                    _ => Ok(StreamEvent::Ping),
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                if self.state.current_kind != Some(BlockKind::Text) {
                    return Ok(StreamEvent::Ping);
                }
                let index = self.state.current_index.unwrap_or(0);
                let delta = value.get_string("delta");
                Ok(StreamEvent::TextDelta { index, text: delta })
            }
            "response.function_call_arguments.delta" => {
                let (item_id, output_index) = tool_event_labels(&value, "item_id");
                let slot = self.state.resolve_tool(&item_id, output_index)?;
                let delta = value.get_string("delta");
                let Some(call) = self.state.tool_calls.get_mut(&slot) else {
                    return Err(tool_identity_error(&format!(
                        "argument delta for unregistered tool block {slot}"
                    )));
                };
                if call.closed {
                    // An empty delta adds nothing, so it stays harmless.
                    // Real bytes after completion cannot be shown to be a
                    // replay of what was already emitted, and dropping them
                    // would silently truncate the call's arguments.
                    if delta.is_empty() {
                        return Ok(StreamEvent::Ping);
                    }
                    return Err(ProviderError::new(
                        ProviderErrorKind::Parse,
                        format!(
                            "Responses stream tool-call argument delta on block {}: {} bytes \
                             arrived after the call was completed",
                            call.stream_index,
                            delta.len()
                        ),
                    ));
                }
                call.arguments.push_str(&delta);
                Ok(StreamEvent::InputJsonDelta {
                    index: call.stream_index,
                    partial_json: delta,
                })
            }
            "response.function_call_arguments.done" => {
                let (item_id, output_index) = tool_event_labels(&value, "item_id");
                let slot = self.state.resolve_tool(&item_id, output_index)?;
                // Without a full snapshot this event carries no more
                // information than the deltas already did. Closing here would
                // discard the fuller payload that `response.output_item.done`
                // still delivers, so always defer to it. Identity is still
                // resolved above, so a stray event cannot pass unnoticed.
                let Some(arguments) = extract_function_call_arguments(&value) else {
                    return Ok(StreamEvent::Ping);
                };
                self.finish_tool_call(slot, Some(arguments))
            }
            "response.reasoning_summary_text.delta" => {
                // Stream reasoning summary text incrementally
                if let Some(ref mut reasoning) = self.state.current_reasoning {
                    let delta = value.get_string("delta");
                    reasoning.summary.push_str(&delta);
                    Ok(StreamEvent::ReasoningDelta {
                        index: reasoning.index,
                        reasoning: delta,
                    })
                } else {
                    Ok(StreamEvent::Ping)
                }
            }
            "response.output_item.done" => {
                // Check if this is a reasoning item with encrypted_content
                let item = value.get("item").unwrap_or(&Value::Null);
                let item_type = item.get_str("type");

                if item_type == "reasoning" {
                    // Extract fields from done event for merging
                    let done_id = item.get_string("id");
                    let done_encrypted = item.get_string("encrypted_content");
                    // Extract summary from done event (array of {type, text} objects)
                    let done_summary = item
                        .get("summary")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .filter(|s| !s.is_empty());

                    // Merge with current_reasoning state if available
                    let (index, id, encrypted_content, summary, had_streamed_summary) =
                        if let Some(reasoning) = self.state.current_reasoning.take() {
                            // Use done event values for encrypted_content
                            let id = if reasoning.id.is_empty() {
                                done_id
                            } else {
                                reasoning.id
                            };
                            // Prefer streamed summary, fall back to done event summary
                            let had_streamed = !reasoning.summary.is_empty();
                            let summary = if had_streamed {
                                Some(reasoning.summary)
                            } else {
                                done_summary
                            };
                            (reasoning.index, id, done_encrypted, summary, had_streamed)
                        } else {
                            // No current_reasoning state - use done event values directly
                            // This shouldn't happen in normal flow, but handle it gracefully
                            let index = self.state.current_index.unwrap_or(0);
                            (index, done_id, done_encrypted, done_summary, false)
                        };

                    // Emit ReasoningCompleted for storage/replay if we have valid data
                    if !id.is_empty() && !encrypted_content.is_empty() {
                        // Use the reasoning item's stored index for ContentBlockCompleted
                        self.state.current_index = None;
                        self.state.current_kind = None;

                        // If summary wasn't streamed but is present in done event,
                        // emit ReasoningDelta first so downstream can avoid duplicating text.
                        if !had_streamed_summary && let Some(ref text) = summary {
                            let reasoning_text = text.clone();
                            self.pending.push_back(StreamEvent::ReasoningCompleted {
                                index,
                                id,
                                encrypted_content,
                                summary,
                            });
                            self.pending.push_back(StreamEvent::ContentBlockCompleted {
                                index,
                                signature: None,
                            });
                            return Ok(StreamEvent::ReasoningDelta {
                                index,
                                reasoning: reasoning_text,
                            });
                        }

                        self.pending.push_back(StreamEvent::ContentBlockCompleted {
                            index,
                            signature: None,
                        });
                        return Ok(StreamEvent::ReasoningCompleted {
                            index,
                            id,
                            encrypted_content,
                            summary,
                        });
                    }
                }

                if item_type == "function_call" {
                    let output_index = value.get("output_index").and_then(Value::as_u64);
                    let item_id = item.get_string("id");
                    let slot = self.state.resolve_tool(&item_id, output_index)?;
                    return self.finish_tool_call(slot, extract_function_call_arguments(item));
                }

                if let Some(index) = self.state.current_index.take() {
                    self.state.current_kind = None;
                    Ok(StreamEvent::ContentBlockCompleted {
                        index,
                        signature: None,
                    })
                } else {
                    Ok(StreamEvent::Ping)
                }
            }
            "response.completed" | "response.done" => {
                let response = value.get("response").unwrap_or(&Value::Null);
                let response_id = response.get_str("id");
                if !response_id.is_empty() {
                    self.last_response_id = Some(response_id.to_string());
                }
                self.warn_on_service_tier_downgrade(response);
                self.terminal_outcome = Some(TerminalOutcome::Completed);
                let usage = usage_from_response(response);

                let stop_reason = if self.state.saw_tool {
                    "tool_use"
                } else {
                    match response.get_str("status") {
                        "incomplete" => "max_tokens",
                        "failed" | "cancelled" => "error",
                        _ => "stop",
                    }
                };

                self.pending.push_back(StreamEvent::MessageStart {
                    model: self.model.clone(),
                    usage: usage.clone(),
                });
                self.pending.push_back(StreamEvent::MessageDelta {
                    stop_reason: Some(stop_reason.to_string()),
                    usage: Some(usage.into()),
                });
                self.pending.push_back(StreamEvent::MessageCompleted);

                Ok(self
                    .pending
                    .pop_front()
                    .expect("pending should contain events"))
            }
            "response.incomplete" => {
                let response = value.get("response").unwrap_or(&Value::Null);
                self.terminal_outcome = Some(TerminalOutcome::Incomplete);
                let usage = usage_from_response(response);
                let stop_reason = response
                    .get("incomplete_details")
                    .and_then(|details| details.get("reason"))
                    .and_then(Value::as_str)
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or("incomplete");

                self.pending.push_back(StreamEvent::MessageStart {
                    model: self.model.clone(),
                    usage: usage.clone(),
                });
                self.pending.push_back(StreamEvent::MessageDelta {
                    stop_reason: Some(stop_reason.to_string()),
                    usage: Some(usage.into()),
                });
                self.pending.push_back(StreamEvent::MessageCompleted);

                Ok(self
                    .pending
                    .pop_front()
                    .expect("pending should contain events"))
            }
            "response.failed" => {
                let response = value.get("response").unwrap_or(&Value::Null);
                let error = response.get("error").unwrap_or(&Value::Null);
                let error_type = error
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("response_failed");
                let message = error_message_from_payload(error, &["message"]);
                Err(ProviderError::api_error(error_type, &message))
            }
            "error" => {
                let error_type = value
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error")
                    .to_string();
                let message = error_message_from_payload(&value, &["message"]);
                Ok(StreamEvent::Error {
                    error_type,
                    message,
                })
            }
            _ => Ok(StreamEvent::Ping),
        }
    }
}

/// SSE parser for `OpenAI` Responses API streaming.
///
/// Owns SSE framing and delegates JSON event payloads to a `ResponsesEventMapper`.
pub struct ResponsesSseParser<S> {
    inner: EventStream<S>,
    mapper: ResponsesEventMapper,
    finished: bool,
}

impl<S> ResponsesSseParser<S> {
    pub fn new(stream: S, model: String) -> Self
    where
        S: Eventsource,
    {
        Self {
            inner: stream.eventsource(),
            mapper: ResponsesEventMapper::new(model),
            finished: false,
        }
    }

    /// Records the `service_tier` asked for, so a silent downgrade is logged.
    #[must_use]
    pub fn with_requested_service_tier(mut self, tier: Option<String>) -> Self {
        self.mapper = self.mapper.with_requested_service_tier(tier);
        self
    }
}

impl<S, E> Stream for ResponsesSseParser<S>
where
    S: Stream<Item = std::result::Result<bytes::Bytes, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
{
    type Item = ProviderResult<StreamEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;

        loop {
            if self.finished {
                return Poll::Ready(None);
            }
            if let Some(event) = self.mapper.pop() {
                return Poll::Ready(Some(Ok(event)));
            }
            if self.mapper.terminal_outcome().is_some() {
                self.finished = true;
                return Poll::Ready(None);
            }

            let inner = Pin::new(&mut self.inner);
            match inner.poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => {
                    if let Err(err) = self.mapper.push_json(&event.data) {
                        self.finished = true;
                        return Poll::Ready(Some(Err(err)));
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(map_event_stream_error(e))));
                }
                Poll::Ready(None) => {
                    self.finished = true;
                    return Poll::Ready(Some(Err(ProviderError::transport(
                        "Responses stream closed before a terminal event",
                    ))));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures_util::stream;
    use serde_json::json;

    use super::*;

    fn mapper() -> ResponsesEventMapper {
        ResponsesEventMapper::new("gpt-5.3-codex-spark".to_string())
    }

    #[test]
    fn response_usage_separates_uncached_read_and_write_tokens() {
        let usage = usage_from_response(&json!({
            "usage": {
                "input_tokens": 20,
                "output_tokens": 7,
                "input_tokens_details": {
                    "cached_tokens": 2,
                    "cache_write_tokens": 3
                }
            }
        }));

        assert_eq!(usage.input_tokens, 15);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.cache_read_input_tokens, 2);
        assert_eq!(usage.cache_creation_input_tokens, 3);
    }

    #[test]
    fn response_usage_defaults_missing_cache_details_to_zero() {
        let usage = usage_from_response(&json!({
            "usage": { "input_tokens": 20, "output_tokens": 7 }
        }));

        assert_eq!(usage.input_tokens, 20);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.cache_read_input_tokens, 0);
        assert_eq!(usage.cache_creation_input_tokens, 0);
    }

    #[test]
    fn response_usage_saturates_inconsistent_cache_totals() {
        let usage = usage_from_response(&json!({
            "usage": {
                "input_tokens": 4,
                "input_tokens_details": {
                    "cached_tokens": 3,
                    "cache_write_tokens": 3
                }
            }
        }));

        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.cache_read_input_tokens, 3);
        assert_eq!(usage.cache_creation_input_tokens, 3);
    }

    #[test]
    fn response_incomplete_preserves_reason_and_usage() {
        let mut mapper = mapper();
        let event = mapper
            .map_event(json!({
                "type": "response.incomplete",
                "response": {
                    "status": "incomplete",
                    "incomplete_details": { "reason": "max_tokens" },
                    "usage": {
                        "input_tokens": 20,
                        "output_tokens": 7,
                        "input_tokens_details": {
                            "cached_tokens": 2,
                            "cache_write_tokens": 3
                        }
                    }
                }
            }))
            .unwrap();

        assert!(matches!(
            event,
            StreamEvent::MessageStart {
                usage: Usage {
                    input_tokens: 15,
                    output_tokens: 7,
                    cache_read_input_tokens: 2,
                    cache_creation_input_tokens: 3,
                },
                ..
            }
        ));
        assert!(matches!(
            mapper.pop(),
            Some(StreamEvent::MessageDelta {
                stop_reason: Some(reason),
                ..
            }) if reason == "max_tokens"
        ));
        assert_eq!(mapper.terminal_outcome(), Some(TerminalOutcome::Incomplete));
        assert_eq!(mapper.last_response_id(), None);
    }

    #[test]
    fn response_incomplete_does_not_assume_token_limit() {
        let mut mapper = mapper();
        let _ = mapper
            .map_event(json!({
                "type": "response.incomplete",
                "response": {
                    "incomplete_details": { "reason": "content_filter" }
                }
            }))
            .unwrap();

        assert!(matches!(
            mapper.pop(),
            Some(StreamEvent::MessageDelta {
                stop_reason: Some(reason),
                ..
            }) if reason == "content_filter"
        ));
    }

    #[test]
    fn response_failed_surfaces_provider_error() {
        let mut mapper = mapper();
        let err = mapper
            .map_event(json!({
                "type": "response.failed",
                "response": {
                    "status": "failed",
                    "error": {
                        "code": "server_error",
                        "message": "The model failed to generate a response."
                    }
                }
            }))
            .unwrap_err();

        assert_eq!(err.kind, ProviderErrorKind::ApiError);
        assert_eq!(err.code.as_deref(), Some("server_error"));
        assert!(err.message.contains("failed to generate"));
        assert_eq!(mapper.last_response_id(), None);
    }

    #[tokio::test]
    async fn eof_before_terminal_event_is_retryable() {
        use futures_util::StreamExt;

        let byte_stream = stream::empty::<std::result::Result<bytes::Bytes, std::io::Error>>();
        let mut parser = ResponsesSseParser::new(byte_stream, "gpt-test".to_string());
        let err = parser
            .next()
            .await
            .expect("stream should emit a terminal error")
            .expect_err("premature EOF must fail");

        assert_eq!(err.kind, ProviderErrorKind::Transport);
        assert!(err.is_retryable());
        assert!(parser.next().await.is_none());
    }

    #[tokio::test]
    async fn completed_event_ends_stream_without_waiting_for_eof() {
        use futures_util::StreamExt;

        let sse = bytes::Bytes::from_static(
            b"data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        );
        let byte_stream = stream::iter([Ok::<_, std::io::Error>(sse)]).chain(stream::pending());
        let mut parser = ResponsesSseParser::new(byte_stream, "gpt-test".to_string());

        assert!(matches!(
            parser.next().await,
            Some(Ok(StreamEvent::MessageDelta { .. }))
        ));
        assert!(matches!(
            parser.next().await,
            Some(Ok(StreamEvent::MessageCompleted))
        ));
        assert!(matches!(
            parser.next().await,
            Some(Ok(StreamEvent::MessageStart { .. }))
        ));
        assert!(parser.next().await.is_none());
    }

    #[test]
    fn done_marker_requires_a_terminal_event() {
        let mut mapper = mapper();
        let err = mapper.push_json("[DONE]").unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Transport);

        mapper
            .push_json(r#"{"type":"response.completed","response":{}}"#)
            .unwrap();
        assert!(mapper.push_json("[DONE]").is_ok());
    }

    #[test]
    fn function_call_done_with_arguments_emits_missing_input_delta() {
        let mut mapper = mapper();

        let start = mapper
            .map_event(json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "bash"
                }
            }))
            .unwrap();
        assert!(matches!(start, StreamEvent::ContentBlockStart { .. }));

        let event = mapper
            .map_event(json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "arguments": "{\"command\":\"git status\"}"
                }
            }))
            .unwrap();

        assert!(matches!(
            event,
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None
            }
        ));
        assert!(matches!(
            mapper.pending.pop_front(),
            Some(StreamEvent::InputJsonDelta { ref partial_json, .. }) if partial_json == "{\"command\":\"git status\"}"
        ));
    }

    #[test]
    fn function_call_done_only_emits_remaining_input_after_delta() {
        let mut mapper = mapper();

        let _ = mapper
            .map_event(json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "bash"
                }
            }))
            .unwrap();

        let first = mapper
            .map_event(json!({
                "type": "response.function_call_arguments.delta",
                "delta": "a"
            }))
            .unwrap();
        assert!(matches!(
            first,
            StreamEvent::InputJsonDelta { ref partial_json, .. } if partial_json == "a"
        ));

        let second = mapper
            .map_event(json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "arguments": "abc"
                }
            }))
            .unwrap();

        assert!(matches!(
            second,
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None
            }
        ));
        assert!(matches!(
            mapper.pending.pop_front(),
            Some(StreamEvent::InputJsonDelta { ref partial_json, .. }) if partial_json == "bc"
        ));
    }

    #[test]
    fn function_call_arguments_done_emits_missing_input_and_completes() {
        let mut mapper = mapper();

        let _ = mapper
            .map_event(json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read"
                }
            }))
            .unwrap();

        let done = mapper
            .map_event(json!({
                "type": "response.function_call_arguments.done",
                "arguments": "{\"file_path\":\"Cargo.toml\"}"
            }))
            .unwrap();

        assert!(matches!(
            done,
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None
            }
        ));
        assert!(matches!(
            mapper.pending.pop_front(),
            Some(StreamEvent::InputJsonDelta { ref partial_json, .. }) if partial_json == "{\"file_path\":\"Cargo.toml\"}"
        ));
    }

    #[test]
    fn function_call_output_item_done_after_arguments_done_is_ignored() {
        let mut mapper = mapper();

        let _ = mapper
            .map_event(json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read"
                }
            }))
            .unwrap();

        let _ = mapper
            .map_event(json!({
                "type": "response.function_call_arguments.done",
                "arguments": "{\"file_path\":\"Cargo.toml\"}"
            }))
            .unwrap();
        let _ = mapper.pending.pop_front();

        let event = mapper
            .map_event(json!({
                "type": "response.output_item.done",
                "item": { "type": "function_call" }
            }))
            .unwrap();

        assert!(matches!(event, StreamEvent::Ping));
    }

    #[test]
    fn function_call_arguments_done_without_arguments_waits_for_output_item_done() {
        let mut mapper = mapper();

        let _ = mapper
            .map_event(json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "bash"
                }
            }))
            .unwrap();

        let early_done = mapper
            .map_event(json!({
                "type": "response.function_call_arguments.done"
            }))
            .unwrap();
        assert!(matches!(early_done, StreamEvent::Ping));

        let final_done = mapper
            .map_event(json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "arguments": "{\"command\":\"ls -la\"}"
                }
            }))
            .unwrap();
        assert!(matches!(
            final_done,
            StreamEvent::ContentBlockCompleted {
                index: 0,
                signature: None
            }
        ));
        assert!(matches!(
            mapper.pending.pop_front(),
            Some(StreamEvent::InputJsonDelta { ref partial_json, .. }) if partial_json == "{\"command\":\"ls -la\"}"
        ));
    }

    /// A terminal snapshot that contradicts what was already streamed cannot
    /// be reconciled: the deltas are downstream already. Previously a shorter
    /// snapshot was silently ignored and a longer non-prefix one had its tail
    /// appended, which is the splice shape. Both now fail the stream.
    #[test]
    fn function_call_done_fails_when_snapshot_contradicts_streamed_prefix() {
        // Shorter, equal-length-but-different, longer-diverging, and wholly
        // different snapshots are all contradictions of the streamed "abcd".
        for snapshot in ["abc", "abxd", "abxyz", "zzzzzz"] {
            let mut mapper = mapper();

            let _ = mapper
                .map_event(json!({
                    "type": "response.output_item.added",
                    "item": {
                        "type": "function_call",
                        "id": "fc_1",
                        "call_id": "call_1",
                        "name": "bash"
                    }
                }))
                .unwrap();

            let _ = mapper
                .map_event(json!({
                    "type": "response.function_call_arguments.delta",
                    "delta": "abcd"
                }))
                .unwrap();

            let err = mapper
                .map_event(json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "function_call",
                        "arguments": snapshot
                    }
                }))
                .expect_err("contradicting snapshot must fail the stream");

            assert_eq!(err.kind, ProviderErrorKind::Parse);
            assert!(
                !err.is_retryable(),
                "argument corruption must not be retried, got {err:?}"
            );
            assert!(
                mapper.pending.is_empty(),
                "no arguments may be emitted from a contradicting snapshot"
            );
        }
    }

    // ---- Multi-tool-call routing ----------------------------------------
    //
    // These drive `push_json`/`pop`, the path production uses. Calling
    // `map_event` directly misreports ordering: a terminal event queues the
    // remainder delta *before* the completion is appended, so only the queue
    // shows what a consumer actually sees.

    /// Ordered record of what a consumer observes for each tool block.
    #[derive(Debug, Default)]
    struct ToolTrace {
        ids: HashMap<usize, String>,
        args: HashMap<usize, String>,
        completed: HashMap<usize, usize>,
        completions: Vec<usize>,
        late_delta: Option<usize>,
    }

    impl ToolTrace {
        /// Accumulated arguments for the block started with `tool_id`.
        fn args_of(&self, tool_id: &str) -> &str {
            let index = self
                .ids
                .iter()
                .find(|(_, id)| id.as_str() == tool_id)
                .map_or_else(
                    || panic!("no tool block started with id {tool_id:?}"),
                    |(index, _)| *index,
                );
            self.args.get(&index).map_or("", String::as_str)
        }
    }

    /// Feeds `events` through the real SSE entry point in order.
    fn run_script(events: &[Value]) -> ProviderResult<ToolTrace> {
        let mut mapper = mapper();
        let mut trace = ToolTrace::default();

        for event in events {
            mapper.push_json(&event.to_string())?;
            while let Some(streamed) = mapper.pop() {
                match streamed {
                    StreamEvent::ContentBlockStart {
                        index,
                        id: Some(id),
                        ..
                    } => {
                        trace.ids.insert(index, id);
                    }
                    StreamEvent::InputJsonDelta {
                        index,
                        partial_json,
                    } => {
                        if trace.completed.contains_key(&index) {
                            trace.late_delta = Some(index);
                        }
                        trace.args.entry(index).or_default().push_str(&partial_json);
                    }
                    StreamEvent::ContentBlockCompleted { index, .. } => {
                        *trace.completed.entry(index).or_default() += 1;
                        trace.completions.push(index);
                    }
                    _ => {}
                }
            }
        }

        assert_eq!(
            trace.late_delta, None,
            "arguments arrived after their block was completed"
        );
        for (index, count) in &trace.completed {
            assert_eq!(*count, 1, "block {index} completed {count} times");
        }
        Ok(trace)
    }

    fn added(output_index: u64, item_id: &str, name: &str) -> Value {
        json!({
            "type": "response.output_item.added",
            "output_index": output_index,
            "item": {
                "type": "function_call",
                "id": item_id,
                "call_id": format!("call_{item_id}"),
                "name": name
            }
        })
    }

    fn delta(output_index: u64, item_id: &str, delta: &str) -> Value {
        json!({
            "type": "response.function_call_arguments.delta",
            "output_index": output_index,
            "item_id": item_id,
            "delta": delta
        })
    }

    fn args_done(output_index: u64, item_id: &str, arguments: &str) -> Value {
        json!({
            "type": "response.function_call_arguments.done",
            "output_index": output_index,
            "item_id": item_id,
            "arguments": arguments
        })
    }

    fn item_done(output_index: u64, item_id: &str, arguments: &str) -> Value {
        json!({
            "type": "response.output_item.done",
            "output_index": output_index,
            "item": {
                "type": "function_call",
                "id": item_id,
                "arguments": arguments
            }
        })
    }

    const READ_A: &str = r#"{"file_path":"apps/web/src/views/TranscriptPane.svelte"}"#;
    const READ_B: &str =
        r#"{"file_path":"/home/user/.config/agent/skills/frontend-design/SKILL.md"}"#;

    /// Exact reproduction of the reported corruption. `READ_A` is 56 bytes and
    /// `READ_B` is 72, so slicing B at A's length yielded `esign/SKILL.md"}`,
    /// which was appended to A: valid JSON plus a stray suffix, reported as
    /// "trailing characters at line 1 column 57".
    #[test]
    fn parallel_read_calls_do_not_splice_arguments() {
        assert_eq!(READ_A.len(), 56);
        assert_eq!(READ_B.len(), 72);
        // The shape the old byte-offset slice produced, reproduced here so the
        // regression is pinned to the reported payload rather than paraphrased.
        assert_eq!(&READ_B[READ_A.len()..], r#"esign/SKILL.md"}"#);

        let trace = run_script(&[
            added(0, "fc_a", "read"),
            added(1, "fc_b", "read"),
            delta(1, "fc_b", READ_B),
            delta(0, "fc_a", READ_A),
            args_done(0, "fc_a", READ_A),
            item_done(1, "fc_b", READ_B),
        ])
        .expect("well-formed parallel calls must stream cleanly");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), READ_A);
        assert_eq!(trace.args_of("call_fc_b|fc_b"), READ_B);
        assert_eq!(trace.completions.len(), 2);
    }

    /// Interleaved deltas are not required to trigger the bug: a terminal
    /// event that merely crosses the next call's start is enough, because the
    /// shared slot had already moved on.
    #[test]
    fn delayed_terminal_after_next_call_starts_keeps_its_own_arguments() {
        let trace = run_script(&[
            added(0, "fc_a", "read"),
            delta(0, "fc_a", READ_A),
            added(1, "fc_b", "read"),
            delta(1, "fc_b", READ_B),
            item_done(0, "fc_a", READ_A),
            item_done(1, "fc_b", READ_B),
        ])
        .expect("delayed terminal must resolve to its own call");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), READ_A);
        assert_eq!(trace.args_of("call_fc_b|fc_b"), READ_B);
        // The shared slot had already advanced to the second call, so the
        // first call's terminal completed the wrong block and left the other
        // one open. Both must complete, each exactly once.
        assert_eq!(trace.completions, vec![0, 1]);
    }

    /// Chunked deltas alternating between two calls must each accumulate into
    /// their own block and remain independently parseable.
    #[test]
    fn alternating_partial_deltas_accumulate_per_call() {
        let trace = run_script(&[
            added(0, "fc_a", "read"),
            added(1, "fc_b", "grep"),
            delta(0, "fc_a", r#"{"file_path":"#),
            delta(1, "fc_b", r#"{"pattern":"#),
            delta(0, "fc_a", r#""a.rs"}"#),
            delta(1, "fc_b", r#""needle"}"#),
            args_done(0, "fc_a", r#"{"file_path":"a.rs"}"#),
            args_done(1, "fc_b", r#"{"pattern":"needle"}"#),
        ])
        .expect("alternating deltas must stay separated");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), r#"{"file_path":"a.rs"}"#);
        assert_eq!(trace.args_of("call_fc_b|fc_b"), r#"{"pattern":"needle"}"#);
        for id in ["call_fc_a|fc_a", "call_fc_b|fc_b"] {
            serde_json::from_str::<Value>(trace.args_of(id)).expect("valid JSON per call");
        }
    }

    /// Completion order need not match start order.
    #[test]
    fn reversed_completion_order_preserves_arguments() {
        let trace = run_script(&[
            added(0, "fc_a", "read"),
            added(1, "fc_b", "read"),
            delta(0, "fc_a", READ_A),
            delta(1, "fc_b", READ_B),
            item_done(1, "fc_b", READ_B),
            item_done(0, "fc_a", READ_A),
        ])
        .expect("reversed completion must still route correctly");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), READ_A);
        assert_eq!(trace.args_of("call_fc_b|fc_b"), READ_B);
        assert_eq!(trace.completions, vec![1, 0]);
    }

    /// Both terminal events for both calls: the second is redundant and must
    /// not re-emit arguments or complete the block twice.
    #[test]
    fn duplicate_terminal_events_complete_each_call_once() {
        let trace = run_script(&[
            added(0, "fc_a", "read"),
            added(1, "fc_b", "read"),
            delta(0, "fc_a", READ_A),
            delta(1, "fc_b", READ_B),
            args_done(0, "fc_a", READ_A),
            item_done(0, "fc_a", READ_A),
            args_done(1, "fc_b", READ_B),
            item_done(1, "fc_b", READ_B),
        ])
        .expect("duplicate terminals must be idempotent");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), READ_A);
        assert_eq!(trace.args_of("call_fc_b|fc_b"), READ_B);
        assert_eq!(trace.completions, vec![0, 1]);
    }

    /// Partial streaming plus a terminal snapshot: each call receives only the
    /// remainder of its own arguments.
    #[test]
    fn terminal_snapshot_emits_only_its_own_remainder() {
        let trace = run_script(&[
            added(0, "fc_a", "read"),
            added(1, "fc_b", "grep"),
            delta(1, "fc_b", r#"{"pattern":"needle","#),
            args_done(0, "fc_a", r#"{"file_path":"a.rs"}"#),
            args_done(1, "fc_b", r#"{"pattern":"needle","path":"src"}"#),
        ])
        .expect("remainders must be per call");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), r#"{"file_path":"a.rs"}"#);
        assert_eq!(
            trace.args_of("call_fc_b|fc_b"),
            r#"{"pattern":"needle","path":"src"}"#
        );
    }

    /// Identity resolves from `output_index` alone.
    #[test]
    fn index_only_identity_routes_correctly() {
        let trace = run_script(&[
            added(0, "fc_a", "read"),
            added(1, "fc_b", "read"),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "delta": READ_A
            }),
            json!({
                "type": "response.function_call_arguments.done",
                "output_index": 0,
                "arguments": READ_A
            }),
        ])
        .expect("output_index alone must identify the call");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), READ_A);
        assert_eq!(trace.args_of("call_fc_b|fc_b"), "");
        assert_eq!(trace.completions, vec![0]);
    }

    /// Identity resolves from `item_id` alone, including when the addressed
    /// call is not the most recently opened one.
    #[test]
    fn id_only_identity_routes_to_the_labeled_call() {
        let trace = run_script(&[
            added(0, "fc_a", "read"),
            added(1, "fc_b", "read"),
            json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_a",
                "delta": READ_A
            }),
            json!({
                "type": "response.function_call_arguments.done",
                "item_id": "fc_a",
                "arguments": READ_A
            }),
        ])
        .expect("item_id alone must identify the call");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), READ_A);
        assert_eq!(trace.args_of("call_fc_b|fc_b"), "");
    }

    /// `output_index: 0` must not be confused with "no index".
    #[test]
    fn output_index_zero_is_a_real_identifier() {
        let trace = run_script(&[
            added(0, "fc_a", "read"),
            added(1, "fc_b", "read"),
            delta(1, "fc_b", READ_B),
            delta(0, "fc_a", READ_A),
            item_done(0, "fc_a", READ_A),
            item_done(1, "fc_b", READ_B),
        ])
        .expect("index 0 must resolve");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), READ_A);
        assert_eq!(trace.args_of("call_fc_b|fc_b"), READ_B);
    }

    /// An unlabeled argument event is ambiguous once several calls exist.
    /// Guessing the newest call is what caused the corruption, so this fails.
    #[test]
    fn unlabeled_event_with_multiple_calls_is_rejected() {
        let err = run_script(&[
            added(0, "fc_a", "read"),
            added(1, "fc_b", "read"),
            json!({
                "type": "response.function_call_arguments.delta",
                "delta": READ_A
            }),
        ])
        .expect_err("ambiguous identity must fail rather than guess");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(err.message.contains("unidentified tool argument event"));
    }

    /// Identifiers that resolve to different calls must fail rather than
    /// letting one silently win.
    #[test]
    fn mismatched_identifiers_are_rejected() {
        let err = run_script(&[
            added(0, "fc_a", "read"),
            added(1, "fc_b", "read"),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 1,
                "item_id": "fc_a",
                "delta": READ_A
            }),
        ])
        .expect_err("contradicting identifiers must fail");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(err.message.contains("resolves to tool block"));
    }

    /// An identifier that matches no registered call must not fall back to
    /// some other call.
    #[test]
    fn unknown_identifier_is_rejected() {
        let err = run_script(&[
            added(0, "fc_a", "read"),
            added(1, "fc_b", "read"),
            json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_ghost",
                "delta": READ_A
            }),
        ])
        .expect_err("unknown identity must fail");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(err.message.contains("no tool call registered"));
    }

    /// Rebinding an identifier to a second call would redirect the first
    /// call's arguments, so registration rejects it.
    #[test]
    fn reused_identifier_across_calls_is_rejected() {
        let err = run_script(&[added(0, "fc_a", "read"), added(1, "fc_a", "read")])
            .expect_err("a reused item id must fail");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(err.message.contains("already bound"));
    }

    /// `arguments.done` without a payload must not close a partially streamed
    /// call: `output_item.done` still carries the full arguments.
    #[test]
    fn missing_arguments_done_after_partial_prefix_waits_for_item_done() {
        let trace = run_script(&[
            added(0, "fc_a", "read"),
            delta(0, "fc_a", r#"{"file_path":"#),
            json!({
                "type": "response.function_call_arguments.done",
                "output_index": 0,
                "item_id": "fc_a"
            }),
            item_done(0, "fc_a", r#"{"file_path":"a.rs"}"#),
        ])
        .expect("an empty terminal must defer to the full one");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), r#"{"file_path":"a.rs"}"#);
        assert_eq!(trace.completions, vec![0]);
    }

    /// Multibyte arguments split mid-character across deltas must reconcile
    /// against the terminal snapshot without slicing inside a code point.
    #[test]
    fn multibyte_arguments_reconcile_against_the_snapshot() {
        let full = r#"{"query":"café ☕ 日本語"}"#;
        let split = full.char_indices().nth(12).expect("long enough").0;

        let trace = run_script(&[
            added(0, "fc_a", "memory_search"),
            delta(0, "fc_a", &full[..split]),
            item_done(0, "fc_a", full),
        ])
        .expect("multibyte remainder must reconcile");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), full);
        serde_json::from_str::<Value>(trace.args_of("call_fc_a|fc_a")).expect("valid JSON");
    }

    /// A snapshot that contradicts one call's streamed prefix fails even when
    /// routing is correct and another call is open.
    #[test]
    fn inconsistent_snapshot_fails_even_with_correct_routing() {
        let err = run_script(&[
            added(0, "fc_a", "read"),
            added(1, "fc_b", "read"),
            delta(0, "fc_a", READ_A),
            item_done(0, "fc_a", READ_B),
        ])
        .expect_err("a non-extending snapshot must fail");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(err.message.contains("do not extend"));
    }

    /// Providers that label nothing still work for a single call, which is
    /// the only unambiguous identifier-free case.
    #[test]
    fn single_unlabeled_call_remains_supported() {
        let trace = run_script(&[
            json!({
                "type": "response.output_item.added",
                "item": { "type": "function_call", "id": "", "call_id": "call_1", "name": "read" }
            }),
            json!({
                "type": "response.function_call_arguments.delta",
                "delta": r#"{"file_path":"#
            }),
            json!({
                "type": "response.output_item.done",
                "item": { "type": "function_call", "arguments": r#"{"file_path":"a.rs"}"# }
            }),
        ])
        .expect("identifier-free single-call streams must keep working");

        assert_eq!(trace.args_of("call_1"), r#"{"file_path":"a.rs"}"#);
        assert_eq!(trace.completions, vec![0]);
    }

    /// An identifier supplied by the provider must resolve on its own. A
    /// stale or invented `item_id` previously rode along on a valid
    /// `output_index`, emitting those bytes under whichever call the index
    /// named.
    #[test]
    fn unknown_item_id_with_known_output_index_is_rejected() {
        let err = run_script(&[
            added(0, "fc_a", "read"),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "item_id": "fc_ghost",
                "delta": READ_A
            }),
        ])
        .expect_err("an unknown item id must not ride along on a valid index");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(err.message.contains("no tool call registered for item id"));
    }

    /// The mirror case: a known `item_id` cannot excuse an `output_index`
    /// that matches no registered call.
    #[test]
    fn known_item_id_with_unknown_output_index_is_rejected() {
        let err = run_script(&[
            added(0, "fc_a", "read"),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 7,
                "item_id": "fc_a",
                "delta": READ_A
            }),
        ])
        .expect_err("an unknown output_index must not ride along on a valid id");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(
            err.message
                .contains("no tool call registered for output_index 7")
        );
    }

    /// A labeled argument delta before any tool call was registered is a
    /// protocol violation, not something to swallow.
    #[test]
    fn argument_delta_before_any_registration_is_rejected() {
        let err = run_script(&[json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 0,
            "item_id": "fc_a",
            "delta": READ_A
        })])
        .expect_err("a delta with no registered call must fail");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(err.message.contains("no tool call registered"));
    }

    /// Likewise for a terminal event carrying a payload.
    #[test]
    fn payload_bearing_terminal_before_any_registration_is_rejected() {
        let err = run_script(&[item_done(0, "fc_a", READ_A)])
            .expect_err("a terminal with no registered call must fail");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(err.message.contains("no tool call registered"));
    }

    /// Arguments arriving after completion cannot be shown to be a replay,
    /// and dropping them would silently truncate the call.
    #[test]
    fn nonempty_delta_after_closure_is_rejected() {
        let err = run_script(&[
            added(0, "fc_a", "read"),
            delta(0, "fc_a", READ_A),
            args_done(0, "fc_a", READ_A),
            delta(0, "fc_a", r#"{"extra":true}"#),
        ])
        .expect_err("late argument bytes must fail the stream");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(err.message.contains("after the call was completed"));
    }

    /// An empty delta after closure adds nothing and stays tolerated.
    #[test]
    fn empty_delta_after_closure_is_tolerated() {
        let trace = run_script(&[
            added(0, "fc_a", "read"),
            delta(0, "fc_a", READ_A),
            args_done(0, "fc_a", READ_A),
            delta(0, "fc_a", ""),
        ])
        .expect("an empty late delta is harmless");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), READ_A);
        assert_eq!(trace.completions, vec![0]);
    }

    /// A duplicate terminal repeating exactly what was completed is an
    /// idempotent replay and must stay accepted.
    #[test]
    fn identical_duplicate_terminal_snapshot_is_accepted() {
        let trace = run_script(&[
            added(0, "fc_a", "read"),
            delta(0, "fc_a", READ_A),
            args_done(0, "fc_a", READ_A),
            item_done(0, "fc_a", READ_A),
        ])
        .expect("an identical duplicate terminal must be accepted");

        assert_eq!(trace.args_of("call_fc_a|fc_a"), READ_A);
        assert_eq!(trace.completions, vec![0]);
    }

    /// A duplicate terminal that changes the arguments is contradictory: the
    /// original bytes are already downstream.
    #[test]
    fn changed_duplicate_terminal_snapshot_is_rejected() {
        let err = run_script(&[
            added(0, "fc_a", "read"),
            delta(0, "fc_a", READ_A),
            args_done(0, "fc_a", READ_A),
            item_done(0, "fc_a", READ_B),
        ])
        .expect_err("a changed duplicate terminal must fail");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(err.message.contains("duplicate terminal carries"));
    }

    /// Even a duplicate terminal that *extends* the completed arguments is
    /// rejected: emitting the suffix would place input after the call's own
    /// `ContentBlockCompleted`.
    #[test]
    fn extending_duplicate_terminal_snapshot_is_rejected() {
        let extended = format!("{READ_A}  ");
        let err = run_script(&[
            added(0, "fc_a", "read"),
            delta(0, "fc_a", READ_A),
            args_done(0, "fc_a", READ_A),
            item_done(0, "fc_a", &extended),
        ])
        .expect_err("an extending duplicate terminal must fail");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(err.message.contains("duplicate terminal carries"));
    }

    /// Equal length is not equality: a same-length snapshot with any byte
    /// disagreement is contradictory data.
    #[test]
    fn equal_length_duplicate_terminal_disagreement_is_rejected() {
        let mut altered = READ_A.to_string();
        altered.replace_range(15..16, "X");
        assert_eq!(altered.len(), READ_A.len());
        assert_ne!(altered, READ_A);

        let err = run_script(&[
            added(0, "fc_a", "read"),
            delta(0, "fc_a", READ_A),
            args_done(0, "fc_a", READ_A),
            item_done(0, "fc_a", &altered),
        ])
        .expect_err("an equal-length disagreement must fail");

        assert_eq!(err.kind, ProviderErrorKind::Parse);
        assert!(err.message.contains("duplicate terminal carries"));
    }

    /// Text streamed alongside an open tool call keeps its own block.
    #[test]
    fn tool_call_does_not_consume_the_text_block_slot() {
        let mut mapper = mapper();

        let _ = mapper
            .map_event(json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "message" }
            }))
            .unwrap();
        let _ = mapper.map_event(added(1, "fc_a", "read")).unwrap();

        let event = mapper
            .map_event(json!({
                "type": "response.output_text.delta",
                "output_index": 0,
                "delta": "hello"
            }))
            .unwrap();

        assert!(
            matches!(event, StreamEvent::TextDelta { index: 0, ref text } if text == "hello"),
            "text must keep streaming into its own block, got {event:?}"
        );
    }

    #[test]
    fn response_completed_captures_response_id() {
        let mut mapper = mapper();

        let event = mapper
            .map_event(json!({
                "type": "response.completed",
                "response": { "id": "resp_123", "status": "completed" }
            }))
            .unwrap();

        assert!(matches!(event, StreamEvent::MessageStart { .. }));
        assert_eq!(mapper.last_response_id(), Some("resp_123"));
    }

    /// Transport-level errors mid-stream (socket reset, connection dropped,
    /// etc.) must surface as a retryable `ProviderError`. Mapping them to
    /// `ProviderErrorKind::Parse` would short-circuit `is_retryable()` to
    /// false and incorrectly treat transient socket failures as fatal.
    #[tokio::test]
    async fn transport_error_is_retryable() {
        use futures_util::StreamExt;

        let byte_stream = stream::iter(vec![Err::<bytes::Bytes, std::io::Error>(
            std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "socket closed mid-stream",
            ),
        )]);
        let mut parser = ResponsesSseParser::new(byte_stream, "gpt-5.3-codex-spark".to_string());

        let first = parser
            .next()
            .await
            .expect("stream should yield the transport error");
        let err = first.expect_err("transport failure must surface as Err");

        assert_ne!(
            err.kind,
            ProviderErrorKind::Parse,
            "transport error must not be classified as Parse (non-retryable)",
        );
        assert!(
            err.is_retryable(),
            "transient transport errors must be retryable, got {err:?}",
        );
    }

    /// Invalid UTF-8 in the byte stream is a real protocol/decoding bug, not a
    /// transient transport blip, and MUST stay non-retryable so the engine
    /// surfaces it as a fatal turn failure instead of silently retrying.
    #[tokio::test]
    async fn utf8_error_is_not_retryable() {
        use futures_util::StreamExt;

        let byte_stream = stream::iter(vec![Ok::<bytes::Bytes, std::io::Error>(
            bytes::Bytes::from_static(&[0xF0, 0x9F]),
        )]);
        let mut parser = ResponsesSseParser::new(byte_stream, "gpt-5.3-codex-spark".to_string());

        let first = parser
            .next()
            .await
            .expect("stream should yield the utf8 error");
        let err = first.expect_err("invalid utf-8 must surface as Err");

        assert_eq!(
            err.kind,
            ProviderErrorKind::Parse,
            "utf-8 framing error must stay classified as Parse",
        );
        assert!(
            !err.is_retryable(),
            "utf-8 framing errors must not be retryable, got {err:?}",
        );
    }
}
