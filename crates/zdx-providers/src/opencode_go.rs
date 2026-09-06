//! `OpenCode` Go provider — meta-provider that routes to the appropriate
//! API client based on the model registry hint.

use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::header::{HeaderMap, HeaderValue};
use zdx_types::ToolDefinition;

use crate::anthropic::api::{AnthropicClient, AnthropicConfig};
use crate::anthropic::types::EffortLevel as AnthropicEffortLevel;
use crate::gemini::api::{GeminiClient, GeminiConfig};
use crate::gemini::shared::GeminiThinkingConfig;
use crate::openai::api::{OpenAIClient, OpenAIConfig};
use crate::openai::chat_completions::{OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig};
use crate::shared::merge_system_prompt;
use crate::{ProviderKind, ProviderStream, StreamingProvider};

#[derive(Debug, Clone)]
pub struct OpencodeGoConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub fallback_max_tokens: u32,
    pub thinking_enabled: bool,
    pub thinking_budget_tokens: u32,
    pub thinking_effort: Option<AnthropicEffortLevel>,
    pub gemini_thinking: Option<GeminiThinkingConfig>,
    pub reasoning_effort: Option<String>,
    pub cache_key: Option<String>,
    /// API routing hint from the model registry (e.g. "anthropic-messages").
    pub api_hint: Option<String>,
}

impl OpencodeGoConfig {
    /// Creates a new `OpencodeGoConfig` from environment variables and provided parameters.
    ///
    /// # Errors
    /// Returns an error if the API key or base URL cannot be resolved.
    #[allow(clippy::too_many_arguments)]
    pub fn from_env(
        model: String,
        max_tokens: Option<u32>,
        fallback_max_tokens: u32,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        thinking_enabled: bool,
        thinking_budget_tokens: u32,
        thinking_effort: Option<AnthropicEffortLevel>,
        gemini_thinking: Option<GeminiThinkingConfig>,
        reasoning_effort: Option<String>,
        cache_key: Option<String>,
        api_hint: Option<String>,
    ) -> Result<Self> {
        let api_key = ProviderKind::OpencodeGo.resolve_api_key(config_api_key)?;
        let base_url = ProviderKind::OpencodeGo.resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
            fallback_max_tokens,
            thinking_enabled,
            thinking_budget_tokens,
            thinking_effort,
            gemini_thinking,
            reasoning_effort,
            cache_key,
            api_hint,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoRoute {
    AnthropicMessages,
    OpenAIResponses,
    GoogleGenerativeAI,
    OpenAICompletions,
}

impl GoRoute {
    fn from_registry_api_hint(api: &str) -> Option<Self> {
        match api {
            "anthropic-messages" => Some(Self::AnthropicMessages),
            "openai-responses" => Some(Self::OpenAIResponses),
            "google-generative-ai" => Some(Self::GoogleGenerativeAI),
            "openai-completions" => Some(Self::OpenAICompletions),
            _ => None,
        }
    }
}

fn resolve_go_route(api_hint: Option<&str>) -> GoRoute {
    api_hint
        .and_then(GoRoute::from_registry_api_hint)
        .unwrap_or(GoRoute::OpenAICompletions)
}

/// `OpenCode` Go requires a stable per-conversation id on every request so it
/// can keep a conversation on one prompt cache; requests without it are rejected.
const SESSION_HEADER: &str = "x-opencode-session";

fn session_id(cache_key: Option<&str>) -> String {
    match cache_key {
        Some(key) => format!("thread:{}", URL_SAFE_NO_PAD.encode(key)),
        // Created once per client, outside the request/retry/tool loops.
        None => format!("run:{}", uuid::Uuid::new_v4()),
    }
}

fn session_headers(cache_key: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let value = HeaderValue::from_str(&session_id(cache_key))
        .expect("session id is encoded as header-safe ASCII");
    headers.insert(SESSION_HEADER, value);
    headers
}

/// `OpenCode` Go meta-provider that routes requests to the appropriate API
/// client based on the model registry hint.
pub struct OpencodeGoClient {
    inner: Box<dyn StreamingProvider>,
}

impl OpencodeGoClient {
    /// Creates a new `OpencodeGoClient`, selecting the inner provider based on the registry hint.
    pub fn new(config: OpencodeGoConfig) -> Self {
        let route = resolve_go_route(config.api_hint.as_deref());
        let session_headers = session_headers(config.cache_key.as_deref());
        let inner: Box<dyn StreamingProvider> = match route {
            GoRoute::AnthropicMessages => {
                // Anthropic Messages API — base_url as-is (client appends /v1/messages)
                Box::new(AnthropicClient::new(AnthropicConfig {
                    api_key: config.api_key,
                    base_url: config.base_url,
                    model: config.model,
                    max_tokens: config.max_tokens.unwrap_or(config.fallback_max_tokens),
                    thinking_enabled: config.thinking_enabled,
                    thinking_budget_tokens: config.thinking_budget_tokens,
                    thinking_effort: config.thinking_effort,
                    extra_headers: session_headers,
                }))
            }
            GoRoute::OpenAIResponses => {
                // OpenAI Responses API — base_url as-is (client uses {base}/v1/responses)
                Box::new(OpenAIClient::new(OpenAIConfig {
                    api_key: config.api_key,
                    base_url: config.base_url,
                    model: config.model,
                    max_output_tokens: config.max_tokens,
                    reasoning_effort: config.reasoning_effort,
                    text_verbosity: None,
                    prompt_cache_key: config.cache_key,
                    service_tier: None,
                    websocket: false,
                    extra_headers: session_headers,
                }))
            }
            GoRoute::GoogleGenerativeAI => {
                // Gemini API — append /v1 (client appends /models/{model}:stream...)
                Box::new(GeminiClient::new(GeminiConfig {
                    api_key: config.api_key,
                    base_url: format!("{}/v1", config.base_url),
                    model: config.model,
                    max_output_tokens: config.max_tokens,
                    thinking_config: config.gemini_thinking,
                    extra_headers: session_headers,
                }))
            }
            GoRoute::OpenAICompletions => {
                // Chat Completions — append /v1 (client appends /chat/completions)
                // The OpenCode proxy rejects `reasoning` and `prompt_cache_key`, so omit those.
                // Reasoning models (e.g. Kimi) need `thinking` + `include_reasoning_content`
                // so `reasoning_content` round-trips in assistant messages.
                Box::new(OpenAIChatCompletionsClient::new(
                    OpenAIChatCompletionsConfig {
                        api_key: config.api_key,
                        base_url: format!("{}/v1", config.base_url),
                        model: config.model,
                        max_tokens: config.max_tokens,
                        max_completion_tokens: None,
                        reasoning_effort: None,
                        prompt_cache_key: None,
                        extra_headers: session_headers,
                        include_usage: true,
                        include_reasoning_content: config.thinking_enabled,
                        thinking: config
                            .thinking_enabled
                            .then(|| config.thinking_enabled.into()),
                    },
                ))
            }
        };

        Self { inner }
    }

    /// Sends messages to the appropriate inner provider and returns a stream.
    ///
    /// # Errors
    /// Returns an error if the inner provider request fails.
    pub async fn send_messages_stream(
        &self,
        messages: &[crate::ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let system = merge_system_prompt(system);
        self.inner
            .stream_messages(messages, tools, system.as_deref())
            .await
    }
}

/// Constructs the `OpenCode Go` meta-provider client from the given context.
///
/// # Errors
/// Returns an error if the API key / base URL cannot be resolved from env or config.
pub fn build(
    ctx: &crate::ProviderBuildContext<'_>,
) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    let thinking_budget_tokens = ctx
        .thinking_level
        .compute_reasoning_budget(ctx.max_tokens)
        .unwrap_or(0);
    Ok(Box::new(OpencodeGoClient::new(OpencodeGoConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.thinking_level.is_enabled(),
        thinking_budget_tokens,
        AnthropicEffortLevel::from_thinking_level(ctx.thinking_level, ctx.model),
        Some(GeminiThinkingConfig::from_thinking_level(
            ctx.thinking_level,
            ctx.model,
        )),
        crate::openai::reasoning_effort_from_thinking_level(ctx.thinking_level).map(str::to_owned),
        ctx.cache_key.clone(),
        ctx.api_hint.clone(),
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_encoding_preserves_distinct_thread_ids() {
        let ids = ["foo!", "foo?", "foo", "foo%21", "", "é", "e", "\r\n"];
        let mut sessions = std::collections::HashSet::new();
        for id in ids {
            let headers = session_headers(Some(id));
            let session = headers[SESSION_HEADER].to_str().unwrap();
            assert_eq!(session, session_id(Some(id)));
            assert!(sessions.insert(session.to_owned()));
            assert_eq!(
                URL_SAFE_NO_PAD
                    .decode(session.strip_prefix("thread:").unwrap())
                    .unwrap(),
                id.as_bytes()
            );
        }
    }

    #[test]
    fn test_route_from_registry_api_hint() {
        assert_eq!(
            GoRoute::from_registry_api_hint("openai-responses"),
            Some(GoRoute::OpenAIResponses)
        );
        assert_eq!(
            GoRoute::from_registry_api_hint("anthropic-messages"),
            Some(GoRoute::AnthropicMessages)
        );
        assert_eq!(GoRoute::from_registry_api_hint("unknown"), None);
    }

    #[test]
    fn test_resolve_route_defaults_to_openai_completions_when_missing_hint() {
        assert_eq!(resolve_go_route(None), GoRoute::OpenAICompletions);
    }
}
