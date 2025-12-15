//! Interactive chat module for ZDX.
//!
//! Provides a REPL-style chat interface that maintains conversation history.

use anyhow::Result;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use crate::config::Config;
use crate::providers::anthropic::{
    AnthropicClient, AnthropicConfig, ChatContentBlock, ChatMessage, ContentBlock,
};
use crate::session::{Session, SessionEvent};
use crate::tools::{self, ToolContext, ToolResult};

const QUIT_COMMAND: &str = ":q";
const PROMPT_PREFIX: &str = "you> ";
const ASSISTANT_PREFIX: &str = "assistant> ";

/// Runs the interactive chat loop.
///
/// Reads user input from `input`, writes responses to `output`.
/// Exits on `:q` command or EOF.
#[allow(dead_code)] // Useful for testing
pub async fn run_chat<R, W>(
    input: R,
    mut output: W,
    config: &Config,
    session: Option<Session>,
    root: PathBuf,
) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    let anthropic_config = AnthropicConfig::from_env(config.model.clone(), config.max_tokens)?;
    let client = AnthropicClient::new(anthropic_config);

    let tool_ctx = ToolContext::new(root.canonicalize().unwrap_or(root));
    run_chat_with_client(input, &mut output, &client, session, &tool_ctx).await
}

/// Runs the chat loop with a provided client (for testing).
#[allow(dead_code)] // Useful for testing
pub async fn run_chat_with_client<R, W>(
    input: R,
    output: &mut W,
    client: &AnthropicClient,
    session: Option<Session>,
    tool_ctx: &ToolContext,
) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    run_chat_with_history(input, output, client, session, Vec::new(), tool_ctx).await
}

/// Runs the chat loop with pre-loaded history.
pub async fn run_chat_with_history<R, W>(
    input: R,
    output: &mut W,
    client: &AnthropicClient,
    session: Option<Session>,
    initial_history: Vec<ChatMessage>,
    tool_ctx: &ToolContext,
) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    let mut history: Vec<ChatMessage> = initial_history;
    let tools = tools::all_tools();

    for line in input.lines() {
        let line = line?;
        let trimmed = line.trim();

        // Handle quit command
        if trimmed == QUIT_COMMAND {
            writeln!(output, "Goodbye!")?;
            break;
        }

        // Skip empty lines
        if trimmed.is_empty() {
            write!(output, "{}", PROMPT_PREFIX)?;
            output.flush()?;
            continue;
        }

        // Add user message to history
        history.push(ChatMessage::user(trimmed));

        // Log user message to session
        if let Some(ref s) = session
            && let Err(e) = s.append(&SessionEvent::user_message(trimmed))
        {
            writeln!(output, "Warning: Failed to save session: {}", e)?;
        }

        // Tool loop - keep going until we get a final response
        let final_text = loop {
            match client.send_messages(&history, &tools).await {
                Ok(response) => {
                    if response.has_tool_use() {
                        // Process tool calls
                        let tool_results = execute_tools(&response, tool_ctx);

                        // Add assistant's response (with tool_use blocks) to history
                        let assistant_blocks = response_to_blocks(&response);
                        history.push(ChatMessage::assistant_blocks(assistant_blocks));

                        // Add tool results as user message
                        history.push(ChatMessage::tool_results(tool_results));

                        // Continue the loop for the next response
                        continue;
                    }

                    // No tool use - we have the final response
                    break response.text().unwrap_or_default();
                }
                Err(e) => {
                    writeln!(output, "Error: {}", e)?;
                    // Remove the failed user message from history
                    history.pop();
                    break String::new();
                }
            }
        };

        if !final_text.is_empty() {
            writeln!(output, "{}{}", ASSISTANT_PREFIX, final_text)?;

            // Log assistant response to session
            if let Some(ref s) = session
                && let Err(e) = s.append(&SessionEvent::assistant_message(&final_text))
            {
                writeln!(output, "Warning: Failed to save session: {}", e)?;
            }

            history.push(ChatMessage::assistant(final_text));
        }

        write!(output, "{}", PROMPT_PREFIX)?;
        output.flush()?;
    }

    Ok(())
}

/// Executes all tool calls from a response.
fn execute_tools(
    response: &crate::providers::anthropic::AssistantResponse,
    ctx: &ToolContext,
) -> Vec<ToolResult> {
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
fn response_to_blocks(
    response: &crate::providers::anthropic::AssistantResponse,
) -> Vec<ChatContentBlock> {
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

/// Runs the chat loop with stdin/stdout.
pub async fn run_interactive_chat(
    config: &Config,
    session: Option<Session>,
    root: PathBuf,
) -> Result<()> {
    run_interactive_chat_with_history(config, session, Vec::new(), root).await
}

/// Runs the interactive chat loop with pre-loaded history.
pub async fn run_interactive_chat_with_history(
    config: &Config,
    session: Option<Session>,
    history: Vec<ChatMessage>,
    root: PathBuf,
) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    let anthropic_config = AnthropicConfig::from_env(config.model.clone(), config.max_tokens)?;
    let client = AnthropicClient::new(anthropic_config);

    let tool_ctx = ToolContext::new(root.canonicalize().unwrap_or(root));

    writeln!(stdout, "ZDX Chat (type :q to quit)")?;
    if let Some(ref s) = session {
        writeln!(stdout, "Session: {}", s.id)?;
    }
    if !history.is_empty() {
        writeln!(stdout, "Loaded {} previous messages", history.len())?;
    }
    write!(stdout, "{}", PROMPT_PREFIX)?;
    stdout.flush()?;

    run_chat_with_history(stdin.lock(), &mut stdout, &client, session, history, &tool_ctx).await
}
