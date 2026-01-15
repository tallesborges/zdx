//! Gemini SSE stream parser.
//!
//! Shared parser for both Gemini API (API key) and Cloud Code Assist (OAuth).

use std::collections::{HashSet, VecDeque};
use std::pin::Pin;

use anyhow::{Result, anyhow};
use eventsource_stream::{EventStream, Eventsource};
use futures_util::Stream;
use serde_json::Value;
use uuid::Uuid;

use crate::providers::{ContentBlockType, StreamEvent, Usage};

/// Gemini SSE stream parser.
///
/// Parses Server-Sent Events from Gemini API responses and converts them
/// to normalized `StreamEvent`s.
pub struct GeminiSseParser<S> {
    inner: EventStream<S>,
    model: String,
    run_id: String,
    tool_id_prefix: String,
    pending: VecDeque<StreamEvent>,
    next_index: usize,
    text_index: Option<usize>,
    last_text: String,
    saw_tool: bool,
    emitted_tool_calls: HashSet<String>,
    final_usage: Option<Usage>,
    final_finish_reason: Option<String>,
    emitted_done: bool,
}

impl<S> GeminiSseParser<S> {
    /// Creates a new parser with custom tool ID prefix.
    ///
    /// The tool ID prefix is used to distinguish tools from different providers
    /// (e.g., "gemini" for API key, "gemini-cli" for OAuth).
    pub fn new(stream: S, model: String, tool_id_prefix: &str) -> Self
    where
        S: Eventsource,
    {
        Self {
            inner: stream.eventsource(),
            model,
            run_id: Uuid::new_v4().to_string(),
            tool_id_prefix: tool_id_prefix.to_string(),
            pending: VecDeque::new(),
            next_index: 0,
            text_index: None,
            last_text: String::new(),
            saw_tool: false,
            emitted_tool_calls: HashSet::new(),
            final_usage: None,
            final_finish_reason: None,
            emitted_done: false,
        }
    }

    fn handle_event_data(&mut self, data: &str) -> Result<()> {
        let trimmed = data.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            return Ok(());
        }

        let value = serde_json::from_str::<Value>(trimmed)
            .map_err(|err| anyhow!("Failed to parse SSE JSON: {}", err))?;
        self.handle_chunk(value)
    }

    fn handle_chunk(&mut self, value: Value) -> Result<()> {
        let payload = value.get("response").unwrap_or(&value);

        if let Some(error) = value.get("error").or_else(|| payload.get("error")) {
            let error_type = error
                .get("status")
                .or_else(|| error.get("code"))
                .and_then(|v| v.as_str())
                .unwrap_or("error")
                .to_string();
            let message = error
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            self.pending.push_back(StreamEvent::Error {
                error_type,
                message,
            });
            return Ok(());
        }

        if let Some(usage) = payload
            .get("usageMetadata")
            .or_else(|| payload.get("usage_metadata"))
        {
            let prompt = usage
                .get("promptTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let completion = usage
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cached_from_details = usage
                .get("cacheTokensDetails")
                .and_then(|v| v.as_array())
                .map(|details| {
                    details
                        .iter()
                        .filter_map(|item| item.get("tokenCount").and_then(|v| v.as_u64()))
                        .sum::<u64>()
                })
                .unwrap_or(0);
            let cached = usage
                .get("cachedContentTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(cached_from_details);
            self.final_usage = Some(Usage {
                input_tokens: prompt.saturating_sub(cached),
                output_tokens: completion,
                cache_read_input_tokens: cached,
                cache_creation_input_tokens: 0,
            });
        }

        if let Some(candidates) = payload.get("candidates").and_then(|v| v.as_array())
            && let Some(candidate) = candidates.first()
        {
            if let Some(reason) = candidate.get("finishReason").and_then(|v| v.as_str()) {
                self.final_finish_reason = Some(reason.to_string());
            }

            if let Some(content) = candidate.get("content")
                && let Some(parts) = content.get("parts").and_then(|v| v.as_array())
            {
                let mut combined_text = String::new();

                for part in parts {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        combined_text.push_str(text);
                    }
                }

                if !combined_text.is_empty() {
                    if self.text_index.is_none() {
                        let index = self.next_index;
                        self.next_index += 1;
                        self.text_index = Some(index);
                        self.pending.push_back(StreamEvent::ContentBlockStart {
                            index,
                            block_type: ContentBlockType::Text,
                            id: None,
                            name: None,
                        });
                    }

                    let delta = if combined_text.starts_with(&self.last_text) {
                        combined_text[self.last_text.len()..].to_string()
                    } else {
                        combined_text.clone()
                    };
                    self.last_text = combined_text;
                    if !delta.is_empty() {
                        self.pending.push_back(StreamEvent::TextDelta {
                            index: self.text_index.unwrap_or(0),
                            text: delta,
                        });
                    }
                }

                for part in parts {
                    if let Some(call) = part.get("functionCall") {
                        let name = call.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let args = call.get("args").unwrap_or(&Value::Null);
                        let key = format!("{}:{}", name, args);
                        if self.emitted_tool_calls.contains(&key) {
                            continue;
                        }
                        self.emitted_tool_calls.insert(key);

                        let tool_id = format!(
                            "{}-{}-{}",
                            self.tool_id_prefix, self.run_id, self.next_index
                        );
                        let index = self.next_index;
                        self.next_index += 1;
                        self.saw_tool = true;

                        self.pending.push_back(StreamEvent::ContentBlockStart {
                            index,
                            block_type: ContentBlockType::ToolUse,
                            id: Some(tool_id.clone()),
                            name: Some(name.to_string()),
                        });

                        let args_json = if args.is_null() {
                            "{}".to_string()
                        } else {
                            serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string())
                        };
                        self.pending.push_back(StreamEvent::InputJsonDelta {
                            index,
                            partial_json: args_json,
                        });
                        self.pending
                            .push_back(StreamEvent::ContentBlockCompleted { index });
                    }
                }
            }
        }

        if let Some(reason) = self.final_finish_reason.clone()
            && !self.emitted_done
        {
            self.emitted_done = true;

            if let Some(index) = self.text_index.take() {
                self.pending
                    .push_back(StreamEvent::ContentBlockCompleted { index });
            }

            let usage = self.final_usage.clone().unwrap_or_default();
            let stop_reason = if self.saw_tool {
                Some("tool_use".to_string())
            } else {
                Some(map_finish_reason(&reason))
            };

            self.pending.push_back(StreamEvent::MessageStart {
                model: self.model.clone(),
                usage: usage.clone(),
            });
            self.pending.push_back(StreamEvent::MessageDelta {
                stop_reason,
                usage: Some(usage),
            });
            self.pending.push_back(StreamEvent::MessageCompleted);
        }

        Ok(())
    }
}

impl<S, E> Stream for GeminiSseParser<S>
where
    S: Stream<Item = std::result::Result<bytes::Bytes, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
{
    type Item = Result<StreamEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;

        loop {
            if let Some(event) = self.pending.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }

            let inner = Pin::new(&mut self.inner);
            match inner.poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => {
                    if let Err(err) = self.handle_event_data(&event.data) {
                        return Poll::Ready(Some(Err(err)));
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(anyhow!("SSE stream error: {}", e))));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Maps Gemini finish reasons to normalized stop reasons.
pub fn map_finish_reason(reason: &str) -> String {
    match reason {
        "MAX_TOKENS" | "max_tokens" => "max_tokens".to_string(),
        "STOP" | "stop" => "stop".to_string(),
        other => other.to_lowercase(),
    }
}
