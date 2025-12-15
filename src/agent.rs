//! Agent module for handling prompt execution.

use anyhow::Result;

use crate::config::Config;
use crate::providers::anthropic::{AnthropicClient, AnthropicConfig};
use crate::session::{Session, SessionEvent};

/// Sends a prompt to the LLM and returns the text response.
///
/// If a session is provided, logs the user prompt and assistant response.
pub async fn execute_prompt(
    prompt: &str,
    config: &Config,
    session: Option<&Session>,
) -> Result<String> {
    let anthropic_config = AnthropicConfig::from_env(config.model.clone(), config.max_tokens)?;

    let client = AnthropicClient::new(anthropic_config);

    // Log user message to session
    if let Some(s) = session {
        s.append(&SessionEvent::user_message(prompt))?;
    }

    let response = client.send_message(prompt).await?;

    // Log assistant response to session
    if let Some(s) = session {
        s.append(&SessionEvent::assistant_message(&response))?;
    }

    Ok(response)
}
