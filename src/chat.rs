//! Interactive chat module for ZDX.
//!
//! Provides a REPL-style chat interface that maintains conversation history.
//! Responses are streamed token-by-token for real-time feedback.

use anyhow::{Result, bail};
use futures_util::StreamExt;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use crate::config::Config;
use crate::providers::anthropic::{
    AnthropicClient, AnthropicConfig, ChatContentBlock, ChatMessage, StreamEvent,
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
    let system_prompt = crate::context::build_effective_system_prompt(config, &root)?;

    let tool_ctx = ToolContext::new(root.canonicalize().unwrap_or(root));
    run_chat_with_client(
        input,
        &mut output,
        &client,
        session,
        &tool_ctx,
        system_prompt.as_deref(),
    )
    .await
}

/// Runs the chat loop with a provided client (for testing).
#[allow(dead_code)] // Useful for testing
pub async fn run_chat_with_client<R, W>(
    input: R,
    output: &mut W,
    client: &AnthropicClient,
    session: Option<Session>,
    tool_ctx: &ToolContext,
    system_prompt: Option<&str>,
) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    run_chat_with_history(
        input,
        output,
        client,
        session,
        Vec::new(),
        tool_ctx,
        system_prompt,
    )
    .await
}

/// Runs the chat loop with pre-loaded history.
pub async fn run_chat_with_history<R, W>(
    input: R,
    output: &mut W,
    client: &AnthropicClient,
    session: Option<Session>,
    initial_history: Vec<ChatMessage>,
    tool_ctx: &ToolContext,
    system_prompt: Option<&str>,
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

        // Tool loop with streaming - keep going until we get a final response
        let final_text = loop {
            match stream_response(output, client, &history, &tools, tool_ctx, system_prompt).await {
                Ok(StreamResult::FinalText(text)) => break text,
                Ok(StreamResult::ToolUse { assistant_blocks, tool_results }) => {
                    // Add assistant's response (with tool_use blocks) to history
                    history.push(ChatMessage::assistant_blocks(assistant_blocks));
                    // Add tool results as user message
                    history.push(ChatMessage::tool_results(tool_results));
                    // Continue the loop for the next response
                    continue;
                }
                Err(e) => {
                    if e.downcast_ref::<crate::interrupt::InterruptedError>().is_some() {
                        if let Some(ref s) = session {
                            let _ = s.append(&SessionEvent::interrupted());
                        }
                        crate::interrupt::reset();
                    } else {
                        writeln!(output, "Error: {}", e)?;
                    }
                    // Remove the failed user message from history
                    history.pop();
                    break String::new();
                }
            }
        };

        if !final_text.is_empty() {
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

/// Result of streaming a single response.
enum StreamResult {
    /// Final text response (no tool use).
    FinalText(String),
    /// Tool use requested - contains blocks for history and results to send.
    ToolUse {
        assistant_blocks: Vec<ChatContentBlock>,
        tool_results: Vec<ToolResult>,
    },
}

/// Builder for accumulating tool use data from streaming events.
#[derive(Debug)]
struct ToolUseBuilder {
    index: usize,
    id: String,
    name: String,
    input_json: String,
}

/// Streams a single response from the API, handling tool use detection.
async fn stream_response<W: Write>(
    output: &mut W,
    client: &AnthropicClient,
    history: &[ChatMessage],
    tools: &[crate::tools::ToolDefinition],
    tool_ctx: &ToolContext,
    system_prompt: Option<&str>,
) -> Result<StreamResult> {
    let mut stream = client
        .send_messages_stream(history, tools, system_prompt)
        .await?;

    // State for accumulating the current response
    let mut full_text = String::new();
    let mut tool_uses: Vec<ToolUseBuilder> = Vec::new();
    let mut stop_reason: Option<String> = None;
    let mut printed_prefix = false;

    // Process stream events
    while let Some(event_result) = stream.next().await {
        if crate::interrupt::is_interrupted() {
            return Err(crate::interrupt::InterruptedError.into());
        }
        let event = event_result?;

        match event {
            StreamEvent::TextDelta { text, .. } => {
                if !text.is_empty() {
                    // Print prefix before first text
                    if !printed_prefix {
                        write!(output, "{}", ASSISTANT_PREFIX)?;
                        printed_prefix = true;
                    }
                    write!(output, "{}", text)?;
                    output.flush()?;
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

        // Execute tools and get results
        let tool_results = execute_tool_uses(&tool_uses, tool_ctx)?;

        return Ok(StreamResult::ToolUse {
            assistant_blocks,
            tool_results,
        });
    }

    // Final newline after streaming completes
    if !full_text.is_empty() {
        writeln!(output)?;
    }

    Ok(StreamResult::FinalText(full_text))
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
        if crate::interrupt::is_interrupted() {
            return Err(crate::interrupt::InterruptedError.into());
        }
        eprint!("⚙ Running {}...", tu.name);
        let _ = std::io::stderr().flush();

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
    let system_prompt = crate::context::build_effective_system_prompt(config, &root)?;

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

    run_chat_with_history(
        stdin.lock(),
        &mut stdout,
        &client,
        session,
        history,
        &tool_ctx,
        system_prompt.as_deref(),
    )
    .await
}
