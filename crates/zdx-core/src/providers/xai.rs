//! xAI provider (Grok) using OpenAI-compatible APIs.

use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue};

use crate::providers::openai::chat_completions::{
    OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig,
};
use crate::providers::openai::responses::{ResponsesConfig, send_responses_stream};
use crate::providers::shared::merge_system_prompt;
use crate::providers::{ChatMessage, ProviderKind, ProviderStream};
use crate::tools::ToolDefinition;

const RESPONSES_PATH: &str = "/responses";

/// `xAI` API configuration.
#[derive(Debug, Clone)]
pub struct XaiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub thinking_enabled: bool,
}

impl XaiConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `XAI_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `XAI_API_KEY` (fallback if not in config)
    /// - `XAI_BASE_URL` (optional)
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
        let api_key = ProviderKind::Xai.resolve_api_key(config_api_key)?;
        let base_url = ProviderKind::Xai.resolve_base_url(config_base_url)?;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XaiApiRoute {
    ChatCompletions,
    Responses,
}

fn route_for_model(model: &str) -> XaiApiRoute {
    if uses_responses_api(model) {
        XaiApiRoute::Responses
    } else {
        XaiApiRoute::ChatCompletions
    }
}

fn uses_responses_api(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "grok-4.20-experimental-beta-0304-reasoning"
            | "grok-4.20-experimental-beta-0304-non-reasoning"
    )
}

struct XaiResponsesClient {
    api_key: String,
    config: ResponsesConfig,
    http: reqwest::Client,
}

impl XaiResponsesClient {
    fn new(config: XaiConfig) -> Self {
        Self {
            api_key: config.api_key,
            config: ResponsesConfig {
                base_url: config.base_url,
                path: RESPONSES_PATH.to_string(),
                model: config.model,
                max_output_tokens: config.max_tokens,
                reasoning_effort: None,
                reasoning_summary: None,
                instructions: None,
                text_verbosity: None,
                store: Some(false),
                include: None,
                stream_options: None,
                prompt_cache_key: config.prompt_cache_key,
                parallel_tool_calls: Some(true),
                tool_choice: Some("auto".to_string()),
                truncation: None,
            },
            http: reqwest::Client::new(),
        }
    }

    async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        send_responses_stream(
            &self.http,
            &self.config,
            build_headers(&self.api_key),
            messages,
            tools,
            system,
        )
        .await
    }
}

fn build_headers(api_key: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {api_key}"))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "user-agent",
        HeaderValue::from_static(crate::providers::shared::USER_AGENT),
    );
    headers
}

enum InnerClient {
    ChatCompletions(OpenAIChatCompletionsClient),
    Responses(XaiResponsesClient),
}

/// `xAI` client using OpenAI-compatible APIs.
pub struct XaiClient {
    inner: InnerClient,
}

impl XaiClient {
    pub fn new(config: XaiConfig) -> Self {
        let inner = match route_for_model(&config.model) {
            XaiApiRoute::ChatCompletions => InnerClient::ChatCompletions(
                OpenAIChatCompletionsClient::new(OpenAIChatCompletionsConfig {
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
            ),
            XaiApiRoute::Responses => InnerClient::Responses(XaiResponsesClient::new(config)),
        };

        Self { inner }
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let system = merge_system_prompt(system);
        match &self.inner {
            InnerClient::ChatCompletions(client) => {
                client
                    .send_messages_stream(messages, tools, system.as_deref())
                    .await
            }
            InnerClient::Responses(client) => {
                client
                    .send_messages_stream(messages, tools, system.as_deref())
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{XaiApiRoute, route_for_model, uses_responses_api};

    #[test]
    fn grok_420_single_agent_models_use_responses_api() {
        assert!(uses_responses_api(
            "grok-4.20-experimental-beta-0304-reasoning"
        ));
        assert!(uses_responses_api(
            "grok-4.20-experimental-beta-0304-non-reasoning"
        ));
        assert_eq!(
            route_for_model("grok-4.20-experimental-beta-0304-reasoning"),
            XaiApiRoute::Responses
        );
    }

    #[test]
    fn existing_grok_models_keep_chat_completions() {
        assert!(!uses_responses_api("grok-4-1-fast"));
        assert_eq!(
            route_for_model("grok-code-fast-1"),
            XaiApiRoute::ChatCompletions
        );
    }
}
