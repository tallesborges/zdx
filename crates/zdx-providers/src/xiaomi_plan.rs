//! `XiaomiPlan` provider (Xiaomi `MiMo` Token Plan, OpenAI-compatible Chat Completions).
//!
//! Same wire protocol as `xiaomi` but pointed at the Token Plan subscription endpoint
//! and authenticated with a `tp-` key. After subscribing, users may also paste their
//! exclusive Base URL into `base_url` or `XIAOMI_PLAN_BASE_URL`.

use anyhow::Result;
use reqwest::header::HeaderMap;
use zdx_types::ToolDefinition;

use crate::openai::chat_completions::{OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig};
use crate::shared::merge_system_prompt;
use crate::{ProviderKind, ProviderStream};

/// `XiaomiPlan` API configuration.
#[derive(Debug, Clone)]
pub struct XiaomiPlanConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_enabled: bool,
}

impl XiaomiPlanConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `XIAOMI_PLAN_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `XIAOMI_PLAN_API_KEY` (fallback if not in config)
    /// - `XIAOMI_PLAN_BASE_URL` (optional; overrides the default Token Plan endpoint)
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn from_env(
        model: String,
        max_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        prompt_cache_key: Option<String>,
        thinking_enabled: bool,
    ) -> Result<Self> {
        let api_key = ProviderKind::XiaomiPlan.resolve_api_key(config_api_key)?;
        let base_url = ProviderKind::XiaomiPlan.resolve_base_url(config_base_url)?;

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

/// `XiaomiPlan` client.
pub struct XiaomiPlanClient {
    inner: OpenAIChatCompletionsClient,
}

impl XiaomiPlanClient {
    pub fn new(config: XiaomiPlanConfig) -> Self {
        Self {
            inner: OpenAIChatCompletionsClient::new(OpenAIChatCompletionsConfig {
                api_key: config.api_key,
                base_url: config.base_url,
                model: config.model,
                max_tokens: None,
                max_completion_tokens: config.max_tokens,
                reasoning_effort: None,
                prompt_cache_key: config.prompt_cache_key,
                extra_headers: HeaderMap::new(),
                include_usage: true,
                include_reasoning_content: config.thinking_enabled,
                thinking: Some(config.thinking_enabled.into()),
            }),
        }
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_messages_stream(
        &self,
        messages: &[crate::ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let system = merge_system_prompt(system);
        self.inner
            .send_messages_stream(messages, tools, system.as_deref())
            .await
    }
}

/// Constructs the `XiaomiPlan` client from the given context.
///
/// # Errors
/// Returns an error if the API key / base URL cannot be resolved from env or config.
pub fn build(ctx: &crate::ProviderBuildContext<'_>) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    Ok(Box::new(XiaomiPlanClient::new(XiaomiPlanConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        None,
        ctx.thinking_enabled,
    )?)))
}
