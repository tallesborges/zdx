//! Interactive chat module for ZDX.
//!
//! Provides a REPL-style chat interface that maintains conversation history.

use anyhow::Result;
use std::io::{BufRead, Write};

use crate::config::Config;
use crate::providers::anthropic::{AnthropicClient, AnthropicConfig, ChatMessage};

const QUIT_COMMAND: &str = ":q";
const PROMPT_PREFIX: &str = "you> ";
const ASSISTANT_PREFIX: &str = "assistant> ";

/// Runs the interactive chat loop.
///
/// Reads user input from `input`, writes responses to `output`.
/// Exits on `:q` command or EOF.
pub async fn run_chat<R, W>(input: R, mut output: W, config: &Config) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    let anthropic_config = AnthropicConfig::from_env(config.model.clone(), config.max_tokens)?;
    let client = AnthropicClient::new(anthropic_config);

    run_chat_with_client(input, &mut output, &client).await
}

/// Runs the chat loop with a provided client (for testing).
pub async fn run_chat_with_client<R, W>(
    input: R,
    output: &mut W,
    client: &AnthropicClient,
) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    let mut history: Vec<ChatMessage> = Vec::new();

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

        // Send to API
        match client.send_messages(&history).await {
            Ok(response) => {
                writeln!(output, "{}{}", ASSISTANT_PREFIX, response)?;
                history.push(ChatMessage::assistant(response));
            }
            Err(e) => {
                writeln!(output, "Error: {}", e)?;
                // Remove the failed user message from history
                history.pop();
            }
        }

        write!(output, "{}", PROMPT_PREFIX)?;
        output.flush()?;
    }

    Ok(())
}

/// Runs the chat loop with stdin/stdout.
pub async fn run_interactive_chat(config: &Config) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    writeln!(stdout, "ZDX Chat (type :q to quit)")?;
    write!(stdout, "{}", PROMPT_PREFIX)?;
    stdout.flush()?;

    run_chat(stdin.lock(), stdout, config).await
}
