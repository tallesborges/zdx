//! OpenAI API key provider (Responses API).

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};

use crate::providers::ProviderStream;
use crate::providers::openai_responses::{ResponsesConfig, StreamOptions, send_responses_stream};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const RESPONSES_PATH: &str = "/responses";
const DEFAULT_TEXT_VERBOSITY: &str = "medium";

/// OpenAI API configuration.
#[derive(Debug, Clone)]
pub struct OpenAIConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_output_tokens: u32,
    pub prompt_cache_key: Option<String>,
}

impl OpenAIConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `OPENAI_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `OPENAI_API_KEY` (fallback if not in config)
    /// - `OPENAI_BASE_URL` (optional)
    pub fn from_env(
        model: String,
        max_output_tokens: u32,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        prompt_cache_key: Option<String>,
    ) -> Result<Self> {
        let api_key = resolve_api_key(config_api_key)?;

        let base_url = resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_output_tokens,
            prompt_cache_key,
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
    ) -> Result<ProviderStream> {
        let headers = build_headers(&self.config.api_key);
        let config = ResponsesConfig {
            base_url: self.config.base_url.clone(),
            path: RESPONSES_PATH.to_string(),
            model: self.config.model.clone(),
            max_output_tokens: Some(self.config.max_output_tokens),
            reasoning_effort: None,
            instructions: None,
            text_verbosity: Some(DEFAULT_TEXT_VERBOSITY.to_string()),
            store: Some(false),
            include: None,
            stream_options: Some(StreamOptions {
                include_obfuscation: Some(false),
            }),
            prompt_cache_key: self.config.prompt_cache_key.clone(),
            parallel_tool_calls: Some(true),
            tool_choice: Some("auto".to_string()),
            truncation: None, // Default: "disabled" - fail if context exceeded
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

/// Resolves API key with precedence: config > env.
fn resolve_api_key(config_api_key: Option<&str>) -> Result<String> {
    // Try config value first
    if let Some(key) = config_api_key {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    // Fall back to env var
    std::env::var("OPENAI_API_KEY")
        .context("No API key available. Set OPENAI_API_KEY or api_key in [providers.openai].")
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
