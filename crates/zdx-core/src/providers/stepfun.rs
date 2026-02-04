//! StepFun provider (Step-3.5-Flash) using OpenAI-compatible API.

use anyhow::{Context as _, Result};
use reqwest::header::HeaderMap;

use crate::prompts::STEPFUN_AGENTIC_PROMPT_TEMPLATE;
use crate::providers::openai::chat_completions::{
    OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig,
};
use crate::providers::{ChatMessage, ProviderStream};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://api.stepfun.ai/v1";

/// StepFun API configuration.
#[derive(Debug, Clone)]
pub struct StepfunConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_enabled: bool,
}

impl StepfunConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `STEPFUN_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `STEPFUN_API_KEY` (fallback if not in config)
    /// - `STEPFUN_BASE_URL` (optional)
    pub fn from_env(
        model: String,
        max_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        prompt_cache_key: Option<String>,
        thinking_enabled: bool,
    ) -> Result<Self> {
        let api_key = resolve_api_key(config_api_key)?;
        let base_url = resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
            prompt_cache_key,
            thinking_enabled,
        })
    }
}

/// StepFun client using OpenAI-compatible API.
pub struct StepfunClient {
    inner: OpenAIChatCompletionsClient,
}

impl StepfunClient {
    pub fn new(config: StepfunConfig) -> Self {
        Self {
            inner: OpenAIChatCompletionsClient::new(OpenAIChatCompletionsConfig {
                api_key: config.api_key,
                base_url: config.base_url,
                model: config.model,
                max_tokens: config.max_tokens,
                max_completion_tokens: None,
                reasoning_effort: None,
                prompt_cache_key: config.prompt_cache_key,
                extra_headers: HeaderMap::new(),
                include_usage: true,
                include_reasoning_content: config.thinking_enabled,
                thinking: Some(config.thinking_enabled.into()),
            }),
        }
    }

    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let system = merge_stepfun_system_prompt(system);
        self.inner
            .send_messages_stream(messages, tools, system.as_deref())
            .await
    }
}

/// Merges the StepFun base prompt with the provided system prompt.
///
/// Always includes the StepFun template first, appending any caller-provided system prompt.
fn merge_stepfun_system_prompt(system: Option<&str>) -> Option<String> {
    let base = STEPFUN_AGENTIC_PROMPT_TEMPLATE.trim();
    let merged = match system {
        Some(prompt) if !prompt.trim().is_empty() => {
            format!("{}\n\n{}", base, prompt.trim())
        }
        _ => base.to_string(),
    };
    Some(merged)
}

fn resolve_base_url(config_base_url: Option<&str>) -> Result<String> {
    if let Ok(env_url) = std::env::var("STEPFUN_BASE_URL") {
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
    url::Url::parse(url).with_context(|| format!("Invalid StepFun base URL: {}", url))?;
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
    std::env::var("STEPFUN_API_KEY")
        .context("No API key available. Set STEPFUN_API_KEY or api_key in [providers.stepfun].")
}
