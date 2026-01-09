//! OpenAI API key provider (Responses API).

use std::pin::Pin;

use anyhow::{Context, Result};
use futures_util::Stream;
use reqwest::header::{HeaderMap, HeaderValue};

use crate::providers::StreamEvent;
use crate::providers::openai_responses::{ResponsesConfig, send_responses_stream};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const RESPONSES_PATH: &str = "/responses";

/// OpenAI API configuration.
#[derive(Debug, Clone)]
pub struct OpenAIConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_output_tokens: u32,
}

impl OpenAIConfig {
    /// Creates a new config from environment.
    ///
    /// Environment variables:
    /// - `OPENAI_API_KEY` (required)
    /// - `OPENAI_BASE_URL` (optional)
    pub fn from_env(
        model: String,
        max_output_tokens: u32,
        config_base_url: Option<&str>,
    ) -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY is not set. Set it to use OpenAI API.")?;

        let base_url = resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_output_tokens,
        })
    }
}

/// OpenAI API client.
pub struct OpenAIClient {
    config: OpenAIConfig,
    http: reqwest::Client,
}

impl OpenAIClient {
    pub fn new(config: OpenAIConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub async fn send_messages_stream(
        &self,
        messages: &[crate::providers::ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let headers = build_headers(&self.config.api_key);
        let config = ResponsesConfig {
            base_url: self.config.base_url.clone(),
            path: RESPONSES_PATH.to_string(),
            model: self.config.model.clone(),
            max_output_tokens: Some(self.config.max_output_tokens),
            reasoning_effort: None,
            instructions: None,
            store: Some(false),
            include: None,
        };

        send_responses_stream(&self.http, &config, headers, messages, tools, system).await
    }
}

fn resolve_base_url(config_base_url: Option<&str>) -> Result<String> {
    if let Ok(env_url) = std::env::var("OPENAI_BASE_URL") {
        let trimmed = env_url.trim();
        if !trimmed.is_empty() {
            validate_url(trimmed)?;
            return Ok(trimmed.to_string());
        }
    }

    if let Some(config_url) = config_base_url {
        let trimmed = config_url.trim();
        if !trimmed.is_empty() {
            validate_url(trimmed)?;
            return Ok(trimmed.to_string());
        }
    }

    Ok(DEFAULT_BASE_URL.to_string())
}

fn validate_url(url: &str) -> Result<()> {
    url::Url::parse(url).with_context(|| format!("Invalid OpenAI base URL: {}", url))?;
    Ok(())
}

fn build_headers(api_key: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", api_key))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers
}
