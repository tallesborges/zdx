//! Agent module for handling prompt execution with tool support.

use anyhow::{Result, bail};
use futures_util::StreamExt;
use std::io::{Write, stdout};
use std::path::PathBuf;

use crate::config::Config;
use crate::providers::anthropic::{
    AnthropicClient, AnthropicConfig, AssistantResponse, ChatContentBlock, ChatMessage,
    ContentBlock, StreamEvent,
};
use crate::session::{Session, SessionEvent};
use crate::tools::{self, ToolContext, ToolResult};

/// Options for agent execution.
#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// Root directory for file operations.
    pub root: PathBuf,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
        }
    }
}

/// Sends a prompt to the LLM and returns the text response (non-streaming).
///
/// If a session is provided, logs the user prompt and assistant response.
/// Implements tool loop - if the model requests tools, executes them and continues.
#[allow(dead_code)] // Useful for testing or non-streaming use cases
pub async fn execute_prompt(
    prompt: &str,
    config: &Config,
    session: Option<&Session>,
    options: &AgentOptions,
) -> Result<String> {
    let anthropic_config = AnthropicConfig::from_env(config.model.clone(), config.max_tokens)?;
    let client = AnthropicClient::new(anthropic_config);
    let system_prompt = crate::context::build_effective_system_prompt(config, &options.root)?;

    // Log user message to session
    if let Some(s) = session {
        s.append(&SessionEvent::user_message(prompt))?;
    }

    let tool_ctx = ToolContext::new(options.root.canonicalize().unwrap_or(options.root.clone()));
    let tools = tools::all_tools();

    let mut messages = vec![ChatMessage::user(prompt)];

    // Tool loop - keep going until we get a final response
    loop {
        let response = client
            .send_messages(&messages, &tools, system_prompt.as_deref())
            .await?;

        if response.has_tool_use() {
            // Process tool calls
            let tool_results = execute_tools(&response, &tool_ctx);

            // Add assistant's response (with tool_use blocks) to history
            let assistant_blocks = response_to_blocks(&response);
            messages.push(ChatMessage::assistant_blocks(assistant_blocks));

            // Add tool results as user message
            messages.push(ChatMessage::tool_results(tool_results));

            // Continue the loop for the next response
            continue;
        }

        // No tool use - we have the final response
        let final_text = response.text().unwrap_or_default();

        // Log assistant response to session
        if let Some(s) = session {
            s.append(&SessionEvent::assistant_message(&final_text))?;
        }

        return Ok(final_text);
    }
}

/// Sends a prompt to the LLM and streams text response to stdout.
///
/// If a session is provided, logs the user prompt and final assistant response.
/// Implements tool loop - if the model requests tools, executes them and continues.
/// Returns the complete response text.
pub async fn execute_prompt_streaming(
    prompt: &str,
    config: &Config,
    session: Option<&Session>,
    options: &AgentOptions,
) -> Result<String> {
    let anthropic_config = AnthropicConfig::from_env(config.model.clone(), config.max_tokens)?;
    let client = AnthropicClient::new(anthropic_config);
    let system_prompt = crate::context::build_effective_system_prompt(config, &options.root)?;

    // Log user message to session
    if let Some(s) = session {
        s.append(&SessionEvent::user_message(prompt))?;
    }

    let tool_ctx = ToolContext::new(options.root.canonicalize().unwrap_or(options.root.clone()));
    let tools = tools::all_tools();

    let mut messages = vec![ChatMessage::user(prompt)];
    let mut stdout = stdout();

    // Tool loop - keep going until we get a final response
    loop {
        let mut stream = client
            .send_messages_stream(&messages, &tools, system_prompt.as_deref())
            .await?;

        // State for accumulating the current response
        let mut full_text = String::new();
        let mut tool_uses: Vec<ToolUseBuilder> = Vec::new();
        let mut stop_reason: Option<String> = None;

        // Process stream events
        while let Some(event_result) = stream.next().await {
            let event = event_result?;

            match event {
                StreamEvent::TextDelta { text, .. } => {
                    if !text.is_empty() {
                        print!("{}", text);
                        stdout.flush()?;
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
                    bail!("API error ({}): {}", error_type, message);
                }
                // Ignore other events (Ping, MessageStart, ContentBlockStop, MessageStop)
                _ => {}
            }
        }

        // Check if we have tool use to process
        if stop_reason.as_deref() == Some("tool_use") && !tool_uses.is_empty() {
            // Build the assistant response with tool_use blocks
            let assistant_blocks = build_assistant_blocks(&full_text, &tool_uses)?;
            messages.push(ChatMessage::assistant_blocks(assistant_blocks));

            // Execute tools and get results
            let tool_results = execute_tool_uses(&tool_uses, &tool_ctx)?;
            messages.push(ChatMessage::tool_results(tool_results));

            // Continue the loop for the next response
            continue;
        }

        // Final newline after streaming completes
        if !full_text.is_empty() {
            println!();
        }

        // Log assistant response to session
        if let Some(s) = session {
            s.append(&SessionEvent::assistant_message(&full_text))?;
        }

        return Ok(full_text);
    }
}

/// Builder for accumulating tool use data from streaming events.
#[derive(Debug)]
struct ToolUseBuilder {
    index: usize,
    id: String,
    name: String,
    input_json: String,
}

/// Builds assistant content blocks from accumulated text and tool uses.
fn build_assistant_blocks(
    text: &str,
    tool_uses: &[ToolUseBuilder],
) -> Result<Vec<ChatContentBlock>> {
    let mut blocks = Vec::new();

    // Add text block if any
    if !text.is_empty() {
        blocks.push(ChatContentBlock::Text(text.to_string()));
    }

    // Add tool_use blocks
    for tu in tool_uses {
        let input: serde_json::Value = serde_json::from_str(&tu.input_json)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        blocks.push(ChatContentBlock::ToolUse {
            id: tu.id.clone(),
            name: tu.name.clone(),
            input,
        });
    }

    Ok(blocks)
}

/// Executes tool uses from streaming and returns results.
fn execute_tool_uses(tool_uses: &[ToolUseBuilder], ctx: &ToolContext) -> Result<Vec<ToolResult>> {
    let mut results = Vec::new();

    for tu in tool_uses {
        eprint!("⚙ Running {}...", tu.name);
        std::io::stderr().flush()?;

        let input: serde_json::Value = serde_json::from_str(&tu.input_json)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        let result =
            tools::execute_tool(&tu.name, &tu.id, &input, ctx).unwrap_or_else(|e| ToolResult {
                tool_use_id: tu.id.clone(),
                content: format!("Internal error: {}", e),
                is_error: true,
            });

        eprintln!(" Done.");
        results.push(result);
    }

    Ok(results)
}

/// Executes all tool calls from a response (non-streaming).
#[allow(dead_code)] // Used by execute_prompt
fn execute_tools(response: &AssistantResponse, ctx: &ToolContext) -> Vec<ToolResult> {
    let mut results = Vec::new();

    for tu in response.tool_uses() {
        eprint!("⚙ Running {}...", tu.name);
        let _ = std::io::stderr().flush();

        let result = tools::execute_tool(&tu.name, &tu.id, &tu.input, ctx).unwrap_or_else(|e| {
            ToolResult {
                tool_use_id: tu.id.clone(),
                content: format!("Internal error: {}", e),
                is_error: true,
            }
        });

        eprintln!(" Done.");
        results.push(result);
    }

    results
}

/// Converts response content blocks to chat content blocks (non-streaming).
#[allow(dead_code)] // Used by execute_prompt
fn response_to_blocks(response: &AssistantResponse) -> Vec<ChatContentBlock> {
    response
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => ChatContentBlock::Text(text.clone()),
            ContentBlock::ToolUse(tu) => ChatContentBlock::ToolUse {
                id: tu.id.clone(),
                name: tu.name.clone(),
                input: tu.input.clone(),
            },
        })
        .collect()
}
