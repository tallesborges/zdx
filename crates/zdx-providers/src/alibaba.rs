//! Alibaba providers (OpenAI-compatible Chat Completions over `DashScope` Model Studio).
//!
//! One shared client serves two separate `ProviderKind`s that differ only by
//! endpoint and API key:
//! - `ProviderKind::Alibaba` — International `DashScope` (pay-as-you-go), `ALIBABA_API_KEY`.
//! - `ProviderKind::QwenCode` — Qwen Code coding plan (subscription), `QWEN_CODE_API_KEY`.
//!
//! Both speak the same wire protocol (bearer key + `/v1/chat/completions`), so
//! `build()` branches on `ctx.provider` to resolve the right key/base URL and
//! otherwise reuses the identical client path. Qwen enables reasoning via a
//! top-level `enable_thinking` boolean, sent through `extra_body` only when
//! thinking is on (Qwen does not understand the `thinking: { type }` object).

use std::collections::HashMap;

use anyhow::Result;
use reqwest::header::HeaderMap;
use serde_json::json;
use zdx_types::ToolDefinition;

use crate::openai::chat_completions::{OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig};
use crate::shared::merge_system_prompt;
use crate::{ProviderKind, ProviderStream};

/// Alibaba API configuration (shared by International + Coding Plan).
#[derive(Debug, Clone)]
pub struct AlibabaConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_enabled: bool,
}

impl AlibabaConfig {
    /// Creates a new config from environment for the given Alibaba provider kind.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. the provider's API key env var (`ALIBABA_API_KEY` or `QWEN_CODE_API_KEY`)
    ///
    /// # Errors
    /// Returns an error if the API key / base URL cannot be resolved.
    pub fn from_env(
        provider: ProviderKind,
        model: String,
        max_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        prompt_cache_key: Option<String>,
        thinking_enabled: bool,
    ) -> Result<Self> {
        let api_key = provider.resolve_api_key(config_api_key)?;
        let base_url = provider.resolve_base_url(config_base_url)?;

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

/// Alibaba client (International + Coding Plan share this implementation).
pub struct AlibabaClient {
    inner: OpenAIChatCompletionsClient,
}

impl AlibabaClient {
    pub fn new(config: AlibabaConfig) -> Self {
        let mut extra_body = HashMap::new();
        if config.thinking_enabled {
            extra_body.insert("enable_thinking".to_string(), json!(true));
        }

        Self {
            inner: OpenAIChatCompletionsClient::with_extra_body(
                OpenAIChatCompletionsConfig {
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
                    // Qwen uses the top-level `enable_thinking` flag (via extra_body),
                    // not the `thinking: { type }` object.
                    thinking: None,
                },
                extra_body,
            ),
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

/// Constructs an Alibaba client from the given context.
///
/// Serves both `ProviderKind::Alibaba` and `ProviderKind::QwenCode`;
/// the key/base URL are resolved from `ctx.provider`.
///
/// # Errors
/// Returns an error if the API key / base URL cannot be resolved from env or config.
pub fn build(
    ctx: &crate::ProviderBuildContext<'_>,
) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    Ok(Box::new(AlibabaClient::new(AlibabaConfig::from_env(
        ctx.provider,
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        ctx.thinking_level.is_enabled(),
    )?)))
}
