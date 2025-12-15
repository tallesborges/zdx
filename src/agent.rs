//! Agent module for handling prompt execution with tool support.

use anyhow::Result;
use std::path::PathBuf;

use crate::config::Config;
use crate::providers::anthropic::{
    AnthropicClient, AnthropicConfig, AssistantResponse, ChatContentBlock, ChatMessage,
    ContentBlock,
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

/// Sends a prompt to the LLM and returns the text response.
///
/// If a session is provided, logs the user prompt and assistant response.
/// Implements tool loop - if the model requests tools, executes them and continues.
pub async fn execute_prompt(
    prompt: &str,
    config: &Config,
    session: Option<&Session>,
    options: &AgentOptions,
) -> Result<String> {
    let anthropic_config = AnthropicConfig::from_env(config.model.clone(), config.max_tokens)?;
    let client = AnthropicClient::new(anthropic_config);

    // Log user message to session
    if let Some(s) = session {
        s.append(&SessionEvent::user_message(prompt))?;
    }

    let tool_ctx = ToolContext::new(options.root.canonicalize().unwrap_or(options.root.clone()));
    let tools = tools::all_tools();

    let mut messages = vec![ChatMessage::user(prompt)];

    // Tool loop - keep going until we get a final response
    loop {
        let response = client.send_messages(&messages, &tools).await?;

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

/// Executes all tool calls from a response.
fn execute_tools(response: &AssistantResponse, ctx: &ToolContext) -> Vec<ToolResult> {
    response
        .tool_uses()
        .into_iter()
        .map(|tu| {
            tools::execute_tool(&tu.name, &tu.id, &tu.input, ctx).unwrap_or_else(|e| ToolResult {
                tool_use_id: tu.id.clone(),
                content: format!("Internal error: {}", e),
                is_error: true,
            })
        })
        .collect()
}

/// Converts response content blocks to chat content blocks.
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
