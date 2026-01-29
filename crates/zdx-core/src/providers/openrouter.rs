//! OpenRouter provider (OpenAI-compatible Chat Completions).

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};

use crate::providers::ProviderStream;
use crate::providers::openai::chat_completions::{
    OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig,
};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// OpenRouter API configuration.
#[derive(Debug, Clone)]
pub struct OpenRouterConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub reasoning_effort: Option<String>,
    pub include_openrouter_headers: bool,
}

impl OpenRouterConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `OPENROUTER_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `OPENROUTER_API_KEY` (fallback if not in config)
    /// - `OPENROUTER_BASE_URL` (optional)
    /// - `OPENROUTER_SITE_URL` (optional)
    /// - `OPENROUTER_APP_NAME` (optional)
    pub fn from_env(
        model: String,
        max_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        reasoning_effort: Option<String>,
    ) -> Result<Self> {
        let api_key = resolve_api_key(config_api_key)?;
        let base_url = resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
            reasoning_effort,
            include_openrouter_headers: true,
        })
    }
}

/// OpenRouter client.
pub struct OpenRouterClient {
    inner: OpenAIChatCompletionsClient,
}

impl OpenRouterClient {
    pub fn new(config: OpenRouterConfig) -> Self {
        let extra_headers = build_openrouter_headers(config.include_openrouter_headers);
        let inner = OpenAIChatCompletionsClient::new(OpenAIChatCompletionsConfig {
            api_key: config.api_key,
            base_url: config.base_url,
            model: config.model,
            max_tokens: config.max_tokens,
            reasoning_effort: config.reasoning_effort,
            extra_headers,
            include_usage: true,
            include_reasoning_content: false,
        });

        Self { inner }
    }

    pub async fn send_messages_stream(
        &self,
        messages: &[crate::providers::ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        self.inner
            .send_messages_stream(messages, tools, system)
            .await
    }
}

fn resolve_base_url(config_base_url: Option<&str>) -> Result<String> {
    if let Ok(env_url) = std::env::var("OPENROUTER_BASE_URL") {
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
    url::Url::parse(url).with_context(|| format!("Invalid OpenRouter base URL: {}", url))?;
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
    std::env::var("OPENROUTER_API_KEY").context(
        "No API key available. Set OPENROUTER_API_KEY or api_key in [providers.openrouter].",
    )
}

fn build_openrouter_headers(include_openrouter_headers: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();

    if include_openrouter_headers {
        if let Ok(site_url) = std::env::var("OPENROUTER_SITE_URL")
            && !site_url.trim().is_empty()
        {
            let _ = headers.insert(
                "HTTP-Referer",
                HeaderValue::from_str(site_url.trim())
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
        }
        if let Ok(app_name) = std::env::var("OPENROUTER_APP_NAME")
            && !app_name.trim().is_empty()
        {
            let _ = headers.insert(
                "X-Title",
                HeaderValue::from_str(app_name.trim())
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
        }
    }

    headers
}
