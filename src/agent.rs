//! Agent module for handling prompt execution.

use anyhow::Result;

use crate::config::Config;
use crate::providers::anthropic::{AnthropicClient, AnthropicConfig};

/// Sends a prompt to the LLM and returns the text response.
pub async fn execute_prompt(prompt: &str, config: &Config) -> Result<String> {
    let anthropic_config = AnthropicConfig::from_env(config.model.clone(), config.max_tokens)?;

    let client = AnthropicClient::new(anthropic_config);
    client.send_message(prompt).await
}
