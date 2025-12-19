//! Engine module for UI-agnostic execution.
//!
//! The engine drives the provider + tool loop and emits `EngineEvent`s
//! via async channels. No direct stdout/stderr writes occur in this module.

use std::path::PathBuf;

use anyhow::{Result, bail};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::events::{EngineEvent, ErrorKind};
use crate::providers::anthropic::{
    AnthropicClient, AnthropicConfig, ChatContentBlock, ChatMessage, ProviderError, StreamEvent,
};
use crate::tools::{self, ToolContext, ToolResult};

/// Options for engine execution.
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Root directory for file operations.
    pub root: PathBuf,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
        }
    }
}

/// Channel-based event sender (async, bounded).
///
/// Used with `run_turn` for concurrent rendering and session persistence.
/// Events are wrapped in `Arc` for efficient cloning to multiple consumers.
pub type EventTx = tokio::sync::mpsc::Sender<std::sync::Arc<EngineEvent>>;

/// Channel-based event receiver (async, bounded).
pub type EventRx = tokio::sync::mpsc::Receiver<std::sync::Arc<EngineEvent>>;

/// Default channel capacity for event streams.
pub const DEFAULT_EVENT_CHANNEL_CAPACITY: usize = 64;

/// Creates a bounded event channel with the default capacity.
pub fn create_event_channel() -> (EventTx, EventRx) {
    tokio::sync::mpsc::channel(DEFAULT_EVENT_CHANNEL_CAPACITY)
}

/// Spawns a fan-out task that distributes events to multiple consumers.
///
/// The task receives events from a single source channel and forwards them
/// to multiple downstream channels (e.g., renderer and session persistence).
/// Events are cloned via `Arc` for efficient multi-consumer delivery.
///
/// The task exits when the source channel closes. Downstream channels that
/// close early are silently ignored (the other consumers continue).
///
/// # Example
///
/// ```ignore
/// let (engine_tx, engine_rx) = create_event_channel();
/// let (render_tx, render_rx) = create_event_channel();
/// let (persist_tx, persist_rx) = create_event_channel();
///
/// let fanout = spawn_fanout_task(engine_rx, vec![render_tx, persist_tx]);
///
/// // Engine sends to engine_tx
/// // Renderer receives from render_rx
/// // Persister receives from persist_rx
/// ```
pub fn spawn_fanout_task(mut rx: EventRx, sinks: Vec<EventTx>) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            for sink in &sinks {
                // Clone the Arc and send to each consumer
                // Ignore send errors (consumer may have closed early)
                let _ = sink.send(event.clone()).await;
            }
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
}

impl ToolUseBuilder {
    /// Parses the accumulated JSON input.
    pub fn parse_input(&self) -> Value {
        serde_json::from_str(&self.input_json).unwrap_or(Value::Object(serde_json::Map::new()))
    }
}

/// Sends an error event via the async channel.
/// Returns `true` if sent successfully, `false` if channel closed.
async fn emit_error_async(err: &anyhow::Error, sink: &EventTx) -> bool {
    let event = if let Some(provider_err) = err.downcast_ref::<ProviderError>() {
        EngineEvent::Error {
            kind: provider_err.kind.clone().into(),
            message: provider_err.message.clone(),
            details: provider_err.details.clone(),
        }
    } else {
        EngineEvent::Error {
            kind: ErrorKind::Internal,
            message: err.to_string(),
            details: None,
        }
    };
    sink.send(std::sync::Arc::new(event)).await.is_ok()
}

/// Sends an event via the async channel.
/// Returns `true` if sent successfully, `false` if channel closed.
async fn send_event(sink: &EventTx, event: EngineEvent) -> bool {
    sink.send(std::sync::Arc::new(event)).await.is_ok()
}

/// Runs a single turn of the engine using async channels.
///
/// Events are sent via a bounded `mpsc` channel for concurrent rendering
/// and session persistence.
///
/// Returns the final assistant text and the updated message history.
pub async fn run_turn(
    messages: Vec<ChatMessage>,
    config: &Config,
    options: &EngineOptions,
    system_prompt: Option<&str>,
    sink: EventTx,
) -> Result<(String, Vec<ChatMessage>)> {
    let anthropic_config = AnthropicConfig::from_env(
        config.model.clone(),
        config.max_tokens,
        config.effective_anthropic_base_url(),
    )?;
    let client = AnthropicClient::new(anthropic_config);

    let tool_ctx = ToolContext::with_timeout(
        options.root.canonicalize().unwrap_or(options.root.clone()),
        config.tool_timeout(),
    );
    let tools = tools::all_tools();

    let mut messages = messages;

    // Tool loop - keep going until we get a final response
    loop {
        if crate::interrupt::is_interrupted() {
            let _ = send_event(&sink, EngineEvent::Interrupted).await;
            return Err(crate::interrupt::InterruptedError.into());
        }

        let mut stream = match client
            .send_messages_stream(&messages, &tools, system_prompt)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                emit_error_async(&e, &sink).await;
                return Err(e);
            }
        };

        // State for accumulating the current response
        let mut full_text = String::new();
        let mut tool_uses: Vec<ToolUseBuilder> = Vec::new();
        let mut stop_reason: Option<String> = None;

        // Process stream events
        while let Some(event_result) = stream.next().await {
            if crate::interrupt::is_interrupted() {
                let _ = send_event(&sink, EngineEvent::Interrupted).await;
                return Err(crate::interrupt::InterruptedError.into());
            }

            let event = match event_result {
                Ok(e) => e,
                Err(e) => {
                    emit_error_async(&e, &sink).await;
                    return Err(e);
                }
            };

            match event {
                StreamEvent::TextDelta { text, .. } => {
                    if !text.is_empty() {
                        let _ = send_event(&sink, EngineEvent::AssistantDelta { text: text.clone() }).await;
                        full_text.push_str(&text);
                    }
                }
                StreamEvent::ContentBlockStart {
                    index,
                    block_type,
                    id,
                    name,
                } => {
                    if block_type == "tool_use" {
                        tool_uses.push(ToolUseBuilder {
                            index,
                            id: id.unwrap_or_default(),
                            name: name.unwrap_or_default(),
                            input_json: String::new(),
                        });
                    }
                }
                StreamEvent::InputJsonDelta {
                    index,
                    partial_json,
                } => {
                    if let Some(tu) = tool_uses.iter_mut().find(|t| t.index == index) {
                        tu.input_json.push_str(&partial_json);
                    }
                }
                StreamEvent::MessageDelta {
                    stop_reason: reason,
                } => {
                    stop_reason = reason;
                }
                StreamEvent::Error {
                    error_type,
                    message,
                } => {
                    let provider_err = ProviderError::api_error(&error_type, &message);
                    let _ = send_event(&sink, EngineEvent::Error {
                        kind: ErrorKind::ApiError,
                        message: provider_err.message.clone(),
                        details: provider_err.details.clone(),
                    }).await;
                    bail!("{}", provider_err.message);
                }
                // Ignore other events (Ping, MessageStart, ContentBlockStop, MessageStop)
                _ => {}
            }
        }

        // Check if we have tool use to process
        if stop_reason.as_deref() == Some("tool_use") && !tool_uses.is_empty() {
            // Emit ToolRequested events for all tools before execution
            for tu in &tool_uses {
                let _ = send_event(&sink, EngineEvent::ToolRequested {
                    id: tu.id.clone(),
                    name: tu.name.clone(),
                    input: tu.parse_input(),
                }).await;
            }

            // Build the assistant response with tool_use blocks
            let assistant_blocks = build_assistant_blocks(&full_text, &tool_uses);
            messages.push(ChatMessage::assistant_blocks(assistant_blocks));

            // Execute tools and get results
            let tool_results = execute_tools_async(&tool_uses, &tool_ctx, &sink).await?;
            messages.push(ChatMessage::tool_results(tool_results));

            // Continue the loop for the next response
            continue;
        }

        // Emit final assistant text
        if !full_text.is_empty() {
            let _ = send_event(&sink, EngineEvent::AssistantFinal {
                text: full_text.clone(),
            }).await;
        }

        return Ok((full_text, messages));
    }
}

/// Executes all tool uses and emits events via async channel.
async fn execute_tools_async(
    tool_uses: &[ToolUseBuilder],
    ctx: &ToolContext,
    sink: &EventTx,
) -> Result<Vec<ToolResult>> {
    let mut results = Vec::new();

    for tu in tool_uses {
        if crate::interrupt::is_interrupted() {
            let _ = send_event(sink, EngineEvent::Interrupted).await;
            return Err(crate::interrupt::InterruptedError.into());
        }

        // Emit ToolStarted
        let _ = send_event(sink, EngineEvent::ToolStarted {
            id: tu.id.clone(),
            name: tu.name.clone(),
        }).await;

        let input = tu.parse_input();
        let (output, result) = tools::execute_tool(&tu.name, &tu.id, &input, ctx).await;

        // Emit ToolFinished with structured output
        let _ = send_event(sink, EngineEvent::ToolFinished {
            id: tu.id.clone(),
            result: output,
        }).await;

        results.push(result);
    }

    Ok(results)
}

/// Builds assistant content blocks from accumulated text and tool uses.
fn build_assistant_blocks(text: &str, tool_uses: &[ToolUseBuilder]) -> Vec<ChatContentBlock> {
    let mut blocks = Vec::new();

    // Add text block if any
    if !text.is_empty() {
        blocks.push(ChatContentBlock::Text(text.to_string()));
    }

    // Add tool_use blocks
    for tu in tool_uses {
        blocks.push(ChatContentBlock::ToolUse {
            id: tu.id.clone(),
            name: tu.name.clone(),
            input: tu.parse_input(),
        });
    }

    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies engine emits ToolStarted and ToolFinished events (SPEC §7).
    #[tokio::test]
    async fn test_execute_tools_emits_events() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("test.txt"), "hello").unwrap();

        let ctx = ToolContext::new(temp.path().to_path_buf());
        let tool_uses = vec![ToolUseBuilder {
            index: 0,
            id: "tool1".to_string(),
            name: "read".to_string(),
            input_json: r#"{"path": "test.txt"}"#.to_string(),
        }];

        let (tx, mut rx) = create_event_channel();
        
        // Run in a task so we can collect events
        let handle = tokio::spawn(async move {
            execute_tools_async(&tool_uses, &ctx, &tx).await
        });

        // Collect events
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push((*ev).clone());
        }

        let results = handle.await.unwrap().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(events.len(), 2); // ToolStarted, ToolFinished

        assert!(
            matches!(&events[0], EngineEvent::ToolStarted { id, name }
            if id == "tool1" && name == "read")
        );
        assert!(
            matches!(&events[1], EngineEvent::ToolFinished { id, result }
            if id == "tool1" && result.is_ok())
        );
    }

    /// Verifies channel is properly closed when sender is dropped.
    #[tokio::test]
    async fn test_event_channel_closes_on_sender_drop() {
        let (tx, mut rx) = create_event_channel();
        
        // Send one event then drop sender
        tx.send(std::sync::Arc::new(EngineEvent::AssistantDelta { 
            text: "hello".to_string() 
        })).await.unwrap();
        drop(tx);

        // Should receive the event
        let ev = rx.recv().await.unwrap();
        assert!(matches!(&*ev, EngineEvent::AssistantDelta { text } if text == "hello"));

        // Should get None when channel is closed
        assert!(rx.recv().await.is_none());
    }
}
