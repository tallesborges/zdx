//! Anthropic Claude API client.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Configuration for the Anthropic client.
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
}

impl AnthropicConfig {
    /// Creates a new config from environment and provided settings.
    ///
    /// Environment variables:
    /// - `ANTHROPIC_API_KEY`: Required API key
    /// - `ANTHROPIC_BASE_URL`: Optional base URL override
    pub fn from_env(model: String, max_tokens: u32) -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .context("ANTHROPIC_API_KEY environment variable is not set")?;

        let base_url =
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
        })
    }
}

/// Anthropic API client.
pub struct AnthropicClient {
    config: AnthropicConfig,
    http: reqwest::Client,
}

impl AnthropicClient {
    /// Creates a new Anthropic client with the given configuration.
    pub fn new(config: AnthropicConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Sends a message and returns the assistant's text response.
    pub async fn send_message(&self, prompt: &str) -> Result<String> {
        let request = MessagesRequest {
            model: &self.config.model,
            max_tokens: self.config.max_tokens,
            messages: vec![Message {
                role: "user",
                content: prompt,
            }],
        };

        let url = format!("{}/v1/messages", self.config.base_url);

        let response = self
            .http
            .post(&url)
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Anthropic API")?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            bail!(
                "Anthropic API request failed with status {}: {}",
                status,
                error_body
            );
        }

        let messages_response: MessagesResponse = response
            .json()
            .await
            .context("Failed to parse Anthropic API response")?;

        // Extract text from the first content block
        messages_response
            .content
            .into_iter()
            .find_map(|block| {
                if block.block_type == "text" {
                    Some(block.text)
                } else {
                    None
                }
            })
            .context("No text content in response")
    }
}

#[derive(Debug, Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<Message<'a>>,
}

#[derive(Debug, Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: String,
}
