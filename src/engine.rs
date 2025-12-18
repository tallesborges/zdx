//! Engine module for UI-agnostic execution.
//!
//! The engine drives the provider + tool loop and emits `EngineEvent`s
//! via a callback. No direct stdout/stderr writes occur in this module.

use std::path::PathBuf;

use anyhow::{Result, bail};
use futures_util::StreamExt;
use serde_json::Value;

use crate::config::Config;
use crate::events::EngineEvent;
use crate::providers::anthropic::{
    AnthropicClient, AnthropicConfig, ChatContentBlock, ChatMessage, StreamEvent,
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

/// Event sink type for receiving engine events.
pub type EventSink = Box<dyn FnMut(EngineEvent) + Send>;

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

/// Runs a single turn of the engine: sends messages to the provider,
/// handles tool loops, and emits events via the sink.
///
/// Returns the final assistant text and the updated message history.
pub async fn run_turn(
    messages: Vec<ChatMessage>,
    config: &Config,
    options: &EngineOptions,
    system_prompt: Option<&str>,
    mut sink: EventSink,
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
            sink(EngineEvent::Interrupted);
            return Err(crate::interrupt::InterruptedError.into());
        }

        let mut stream = client
            .send_messages_stream(&messages, &tools, system_prompt)
            .await?;

        // State for accumulating the current response
        let mut full_text = String::new();
        let mut tool_uses: Vec<ToolUseBuilder> = Vec::new();
        let mut stop_reason: Option<String> = None;

        // Process stream events
        while let Some(event_result) = stream.next().await {
            if crate::interrupt::is_interrupted() {
                sink(EngineEvent::Interrupted);
                return Err(crate::interrupt::InterruptedError.into());
            }

            let event = event_result?;

            match event {
                StreamEvent::TextDelta { text, .. } => {
                    if !text.is_empty() {
                        sink(EngineEvent::AssistantDelta { text: text.clone() });
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
                    let error_msg = format!("API error ({}): {}", error_type, message);
                    sink(EngineEvent::Error {
                        message: error_msg.clone(),
                    });
                    bail!("{}", error_msg);
                }
                // Ignore other events (Ping, MessageStart, ContentBlockStop, MessageStop)
                _ => {}
            }
        }

        // Check if we have tool use to process
        if stop_reason.as_deref() == Some("tool_use") && !tool_uses.is_empty() {
            // Emit ToolRequested events for all tools before execution
            for tu in &tool_uses {
                sink(EngineEvent::ToolRequested {
                    id: tu.id.clone(),
                    name: tu.name.clone(),
                    input: tu.parse_input(),
                });
            }

            // Build the assistant response with tool_use blocks
            let assistant_blocks = build_assistant_blocks(&full_text, &tool_uses);
            messages.push(ChatMessage::assistant_blocks(assistant_blocks));

            // Execute tools and get results
            let tool_results = execute_tools(&tool_uses, &tool_ctx, &mut sink).await?;
            messages.push(ChatMessage::tool_results(tool_results));

            // Continue the loop for the next response
            continue;
        }

        // Emit final assistant text
        if !full_text.is_empty() {
            sink(EngineEvent::AssistantFinal {
                text: full_text.clone(),
            });
        }

        return Ok((full_text, messages));
    }
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

/// Executes all tool uses and emits events.
async fn execute_tools(
    tool_uses: &[ToolUseBuilder],
    ctx: &ToolContext,
    sink: &mut EventSink,
) -> Result<Vec<ToolResult>> {
    let mut results = Vec::new();

    for tu in tool_uses {
        if crate::interrupt::is_interrupted() {
            sink(EngineEvent::Interrupted);
            return Err(crate::interrupt::InterruptedError.into());
        }

        // Emit ToolStarted
        sink(EngineEvent::ToolStarted {
            id: tu.id.clone(),
            name: tu.name.clone(),
        });

        let input = tu.parse_input();
        let (output, result) = tools::execute_tool(&tu.name, &tu.id, &input, ctx).await;

        // Emit ToolFinished with structured output
        sink(EngineEvent::ToolFinished {
            id: tu.id.clone(),
            result: output,
        });

        results.push(result);
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Helper to collect events into a vec.
    fn collecting_sink() -> (EventSink, Arc<Mutex<Vec<EngineEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let sink: EventSink = Box::new(move |event| {
            events_clone.lock().unwrap().push(event);
        });
        (sink, events)
    }

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

        let (mut sink, events) = collecting_sink();
        let results = execute_tools(&tool_uses, &ctx, &mut sink).await.unwrap();

        assert_eq!(results.len(), 1);
        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 2); // ToolStarted, ToolFinished

        assert!(
            matches!(&collected[0], EngineEvent::ToolStarted { id, name }
            if id == "tool1" && name == "read")
        );
        assert!(
            matches!(&collected[1], EngineEvent::ToolFinished { id, result }
            if id == "tool1" && result.is_ok())
        );
    }

    /// Verifies ToolFinished is emitted even on tool errors (SPEC §7).
    #[tokio::test]
    async fn test_execute_tools_error_emits_finished() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf());

        let tool_uses = vec![ToolUseBuilder {
            index: 0,
            id: "tool1".to_string(),
            name: "read".to_string(),
            input_json: r#"{"path": "nonexistent.txt"}"#.to_string(),
        }];

        let (mut sink, events) = collecting_sink();
        let results = execute_tools(&tool_uses, &ctx, &mut sink).await.unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);

        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 2);
        assert!(
            matches!(&collected[1], EngineEvent::ToolFinished { result, .. }
            if !result.is_ok())
        );
    }

    /// Verifies Interrupted event is emitted on Ctrl+C (SPEC §7).
    #[tokio::test]
    async fn test_execute_tools_handles_interrupt() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf());

        let tool_uses = vec![ToolUseBuilder {
            index: 0,
            id: "tool1".to_string(),
            name: "read".to_string(),
            input_json: r#"{"path": "test.txt"}"#.to_string(),
        }];

        let (mut sink, events) = collecting_sink();

        // Set interrupt flag
        crate::interrupt::set_interrupted(true);

        let result = execute_tools(&tool_uses, &ctx, &mut sink).await;

        assert!(result.is_err());
        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 1);
        assert!(matches!(&collected[0], EngineEvent::Interrupted));

        // Clean up
        crate::interrupt::set_interrupted(false);
    }
}
