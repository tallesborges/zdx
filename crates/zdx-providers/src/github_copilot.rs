//! GitHub Copilot provider: one subscription, three wire APIs.
//!
//! Copilot serves each model on one API, so the client routes by model
//! (registry `api` hint first, then [`api_for_model`]):
//! - Claude: Anthropic Messages at `{base}/v1/messages`, bearer auth.
//! - GPT-5 and later (except `gpt-5-mini`), Grok, MAI, OSWE: Responses at
//!   `{base}/responses`.
//! - Everything else (Gemini, Kimi, ...): Chat Completions at
//!   `{base}/chat/completions`.
//!
//! The bearer is the short-lived Copilot token from
//! [`crate::oauth::github_copilot`], exchanged again from the stored GitHub
//! token when it expires. The base URL comes from that token (`proxy-ep`),
//! unless `GITHUB_COPILOT_BASE_URL` or the config `base_url` overrides it.
//!
//! Every request carries VS Code Copilot Chat's identity headers plus
//! per-request `X-Initiator` (`user` when the last message is the user's own
//! input, `agent` for tool-result follow-ups), `Openai-Intent:
//! conversation-edits`, and `Copilot-Vision-Request: true` when any message
//! holds an image. Protocol follows Pi (`badlogic/pi-mono`,
//! `packages/ai/src/providers/github-copilot.ts` and
//! `packages/ai/src/api/github-copilot-headers.ts`).

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use zdx_types::{ToolDefinition, ToolResultBlock, ToolResultContent};

use crate::anthropic::api::{AnthropicClient, AnthropicConfig};
use crate::anthropic::types::EffortLevel as AnthropicEffortLevel;
use crate::oauth::github_copilot as oauth_copilot;
use crate::openai::chat_completions::{OpenAIChatCompletionsClient, OpenAIChatCompletionsConfig};
use crate::openai::responses::{ResponsesConfig, send_responses_stream};
use crate::shared::merge_system_prompt;
use crate::{ChatContentBlock, ChatMessage, MessageContent, ProviderKind, ProviderStream};

/// Request label for logs.
const CLIENT_LABEL: &str = "github-copilot";
const RESPONSES_PATH: &str = "/responses";

/// Wire API Copilot serves a model on, in the registry's `api` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopilotApi {
    AnthropicMessages,
    OpenAIResponses,
    OpenAICompletions,
}

impl CopilotApi {
    /// The registry `api` value for this route.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "anthropic-messages",
            Self::OpenAIResponses => "openai-responses",
            Self::OpenAICompletions => "openai-completions",
        }
    }

    fn from_hint(hint: &str) -> Option<Self> {
        match hint {
            "anthropic-messages" => Some(Self::AnthropicMessages),
            "openai-responses" => Some(Self::OpenAIResponses),
            "openai-completions" => Some(Self::OpenAICompletions),
            _ => None,
        }
    }
}

/// The API Copilot serves `model` on, from its id.
///
/// Copilot serves Claude only on Messages, and GPT-5 and later (except
/// `gpt-5-mini`), Grok, MAI, and OSWE only on Responses; the rest use Chat
/// Completions.
#[must_use]
pub fn api_for_model(model: &str) -> CopilotApi {
    let id = model.rsplit(':').next().unwrap_or(model);
    if id.starts_with("claude-") {
        return CopilotApi::AnthropicMessages;
    }
    if let Some(rest) = id.strip_prefix("gpt-") {
        let major: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if major.parse::<u32>().is_ok_and(|n| n >= 5) && !id.starts_with("gpt-5-mini") {
            return CopilotApi::OpenAIResponses;
        }
        return CopilotApi::OpenAICompletions;
    }
    if ["grok-", "mai-", "oswe"]
        .iter()
        .any(|prefix| id.starts_with(prefix))
    {
        return CopilotApi::OpenAIResponses;
    }
    CopilotApi::OpenAICompletions
}

/// Resolves the Copilot token, exchanging the GitHub token again if expired.
///
/// # Errors
/// Returns an error if no credentials are stored or the exchange fails.
pub async fn resolve_access_token(account: Option<&str>) -> Result<String> {
    let creds = oauth_copilot::load_credentials(account)?.ok_or_else(|| {
        anyhow::anyhow!("No GitHub Copilot credentials found. Run `zdx login --github-copilot`.")
    })?;

    let creds = if creds.is_expired() {
        crate::oauth::refresh_locked(
            oauth_copilot::PROVIDER_KEY,
            account,
            |github_token| async move { oauth_copilot::refresh_token(&github_token).await },
        )
        .await
        .context("Failed to refresh the GitHub Copilot token")?
    } else {
        creds
    };

    Ok(creds.access)
}

/// Whether the request is a follow-up the agent sent on its own: the last
/// message is not the user's, or it only carries tool results.
fn initiator(messages: &[ChatMessage]) -> &'static str {
    let Some(last) = messages.last() else {
        return "user";
    };
    if last.role != "user" {
        return "agent";
    }
    match &last.content {
        MessageContent::Text(_) => "user",
        MessageContent::Blocks(blocks) => {
            if blocks
                .iter()
                .any(|block| !matches!(block, ChatContentBlock::ToolResult(_)))
            {
                "user"
            } else {
                "agent"
            }
        }
    }
}

fn has_images(messages: &[ChatMessage]) -> bool {
    messages.iter().any(|message| match &message.content {
        MessageContent::Text(_) => false,
        MessageContent::Blocks(blocks) => blocks.iter().any(|block| match block {
            ChatContentBlock::Image { .. } => true,
            ChatContentBlock::ToolResult(result) => match &result.content {
                ToolResultContent::Text(_) => false,
                ToolResultContent::Blocks(parts) => parts
                    .iter()
                    .any(|part| matches!(part, ToolResultBlock::Image { .. })),
            },
            _ => false,
        }),
    })
}

/// Identity and per-request headers for one Copilot call. Authorization is
/// set by the route.
fn request_headers(messages: &[ChatMessage]) -> HeaderMap {
    let mut headers = oauth_copilot::identity_headers();
    headers.insert("x-initiator", HeaderValue::from_static(initiator(messages)));
    headers.insert(
        "openai-intent",
        HeaderValue::from_static("conversation-edits"),
    );
    if has_images(messages) {
        headers.insert("copilot-vision-request", HeaderValue::from_static("true"));
    }
    headers
}

enum Route {
    Messages(Box<AnthropicClient>),
    Responses {
        config: Box<ResponsesConfig>,
        http: reqwest::Client,
    },
    Completions(Box<OpenAIChatCompletionsClient>),
}

/// Inputs for [`GithubCopilotClient::new`].
#[derive(Debug, Clone)]
pub struct GithubCopilotConfig {
    pub base_url: String,
    pub model: String,
    /// Global `max_tokens`, for the `OpenAI` routes.
    pub max_tokens: Option<u32>,
    /// Effective max tokens, required by the Messages route.
    pub fallback_max_tokens: u32,
    pub thinking_enabled: bool,
    pub thinking_budget_tokens: u32,
    pub thinking_effort: Option<AnthropicEffortLevel>,
    /// Responses `reasoning.effort`; `None` omits reasoning.
    pub reasoning_effort: Option<String>,
    pub cache_key: Option<String>,
    pub api_hint: Option<String>,
    pub account: Option<String>,
}

/// GitHub Copilot client routing each model to the API Copilot serves it on.
pub struct GithubCopilotClient {
    route: Route,
    account: Option<String>,
}

impl GithubCopilotClient {
    pub fn new(config: GithubCopilotConfig) -> Self {
        let api = config
            .api_hint
            .as_deref()
            .and_then(CopilotApi::from_hint)
            .unwrap_or_else(|| api_for_model(&config.model));
        let route = match api {
            CopilotApi::AnthropicMessages => {
                Route::Messages(Box::new(AnthropicClient::new(AnthropicConfig {
                    // Unused: the bearer is resolved per request.
                    api_key: String::new(),
                    base_url: config.base_url,
                    model: config.model,
                    max_tokens: config.max_tokens.unwrap_or(config.fallback_max_tokens),
                    thinking_enabled: config.thinking_enabled,
                    thinking_budget_tokens: config.thinking_budget_tokens,
                    thinking_effort: config.thinking_effort,
                    explicit_thinking_off: false,
                    extra_headers: HeaderMap::new(),
                })))
            }
            CopilotApi::OpenAIResponses => Route::Responses {
                config: Box::new(ResponsesConfig {
                    base_url: config.base_url,
                    path: RESPONSES_PATH.to_string(),
                    model: config.model,
                    max_output_tokens: config.max_tokens,
                    reasoning_summary: config.reasoning_effort.as_ref().map(|_| "auto".into()),
                    include: config
                        .reasoning_effort
                        .as_ref()
                        .map(|_| vec!["reasoning.encrypted_content".to_string()]),
                    reasoning_effort: config.reasoning_effort,
                    instructions: None,
                    text_verbosity: None,
                    store: Some(false),
                    stream_options: None,
                    prompt_cache_key: config.cache_key,
                    parallel_tool_calls: Some(true),
                    tool_choice: Some("auto".to_string()),
                    truncation: None,
                    service_tier: None,
                }),
                http: crate::shared::streaming_http_client(),
            },
            CopilotApi::OpenAICompletions => Route::Completions(Box::new(
                OpenAIChatCompletionsClient::new(OpenAIChatCompletionsConfig {
                    // Unused: the bearer is resolved per request.
                    api_key: String::new(),
                    base_url: config.base_url,
                    model: config.model,
                    max_tokens: config.max_tokens,
                    max_completion_tokens: None,
                    reasoning_effort: None,
                    prompt_cache_key: None,
                    extra_headers: HeaderMap::new(),
                    include_usage: true,
                    include_reasoning_content: false,
                    replay_historical_tool_turns: false,
                    thinking: None,
                }),
            )),
        };
        Self {
            route,
            account: config.account,
        }
    }

    /// Streams one request on the model's route.
    ///
    /// # Errors
    /// Returns an error if the token cannot be resolved or the request fails.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let token = resolve_access_token(self.account.as_deref()).await?;
        let system = merge_system_prompt(system);
        let system = system.as_deref();
        let headers = request_headers(messages);

        match &self.route {
            Route::Messages(client) => {
                client
                    .send_messages_stream_bearer(
                        CLIENT_LABEL,
                        &token,
                        &headers,
                        messages,
                        tools,
                        system,
                    )
                    .await
            }
            Route::Responses { config, http } => {
                let mut headers = headers;
                headers.insert(
                    "authorization",
                    crate::shared::header_value(
                        "GitHub Copilot token",
                        &format!("Bearer {token}"),
                    )?,
                );
                headers.insert("accept", HeaderValue::from_static("text/event-stream"));
                headers.insert("content-type", HeaderValue::from_static("application/json"));
                send_responses_stream(CLIENT_LABEL, http, config, headers, messages, tools, system)
                    .await
            }
            Route::Completions(client) => {
                client
                    .send_messages_stream_with(
                        CLIENT_LABEL,
                        &token,
                        &headers,
                        messages,
                        tools,
                        system,
                    )
                    .await
            }
        }
    }
}

/// Base URL: `GITHUB_COPILOT_BASE_URL`, then config, then the stored token's
/// `proxy-ep`, then the individual-plan default. Reading the stored token
/// here is a cheap file read; a missing login surfaces on the first request.
fn resolve_base_url(config_base_url: Option<&str>, account: Option<&str>) -> Result<String> {
    let from_token = oauth_copilot::load_credentials(account)
        .ok()
        .flatten()
        .and_then(|creds| oauth_copilot::api_base_url(&creds.access));
    crate::shared::resolve_base_url(
        config_base_url,
        ProviderKind::GithubCopilot
            .base_url_env_var()
            .unwrap_or_default(),
        from_token
            .as_deref()
            .unwrap_or(ProviderKind::GithubCopilot.default_base_url()),
        ProviderKind::GithubCopilot.label(),
    )
}

/// Responses `reasoning.effort` for Copilot. Off omits reasoning (Copilot,
/// like Pi, gets no explicit `none`); only GPT models take an effort.
fn responses_reasoning_effort(
    level: zdx_types::config::ThinkingLevel,
    model: &str,
) -> Option<String> {
    if !level.is_enabled() || !model.starts_with("gpt-") {
        return None;
    }
    crate::openai::responses_reasoning_effort(level, model).map(str::to_owned)
}

/// Constructs the GitHub Copilot client from the given context.
///
/// # Errors
/// Returns an error if the base URL override is invalid.
pub fn build(
    ctx: &crate::ProviderBuildContext<'_>,
) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    let base_url = resolve_base_url(ctx.base_url, ctx.account)?;
    let thinking_budget_tokens = ctx
        .thinking_level
        .compute_reasoning_budget(ctx.max_tokens)
        .unwrap_or(0);
    Ok(Box::new(GithubCopilotClient::new(GithubCopilotConfig {
        base_url,
        model: ctx.model.to_string(),
        max_tokens: ctx.config_max_tokens,
        fallback_max_tokens: ctx.max_tokens,
        thinking_enabled: ctx.thinking_level.is_enabled(),
        thinking_budget_tokens,
        thinking_effort: AnthropicEffortLevel::from_thinking_level(ctx.thinking_level, ctx.model),
        reasoning_effort: responses_reasoning_effort(ctx.thinking_level, ctx.model),
        cache_key: ctx.cache_key.clone(),
        api_hint: ctx.api_hint.clone(),
        account: ctx.account.map(ToString::to_string),
    })))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use zdx_types::config::ThinkingLevel;
    use zdx_types::{ToolResult, ToolResultBlock, ToolResultContent};

    use super::*;

    #[test]
    fn models_route_to_the_api_copilot_serves_them_on() {
        for (model, api) in [
            ("claude-sonnet-4.6", CopilotApi::AnthropicMessages),
            ("claude-opus-5", CopilotApi::AnthropicMessages),
            ("gpt-6-luna", CopilotApi::OpenAIResponses),
            ("gpt-5.4-mini", CopilotApi::OpenAIResponses),
            ("gpt-5.3-codex", CopilotApi::OpenAIResponses),
            ("gpt-5-mini", CopilotApi::OpenAICompletions),
            ("gpt-4.1", CopilotApi::OpenAICompletions),
            ("grok-4.7", CopilotApi::OpenAIResponses),
            ("mai-code-1.1-flash", CopilotApi::OpenAIResponses),
            ("mai-code-2", CopilotApi::OpenAIResponses),
            ("mai-code-2-flash", CopilotApi::OpenAIResponses),
            ("gemini-4-carbon", CopilotApi::OpenAICompletions),
            ("kimi-k3", CopilotApi::OpenAICompletions),
            (
                "github-copilot:claude-haiku-4.5",
                CopilotApi::AnthropicMessages,
            ),
        ] {
            assert_eq!(api_for_model(model), api, "{model}");
        }
    }

    #[test]
    fn registry_hint_wins_over_the_model_id_rule() {
        let client = GithubCopilotClient::new(GithubCopilotConfig {
            base_url: "http://mock".to_string(),
            model: "gpt-6-luna".to_string(),
            max_tokens: None,
            fallback_max_tokens: 1024,
            thinking_enabled: false,
            thinking_budget_tokens: 0,
            thinking_effort: None,
            reasoning_effort: None,
            cache_key: None,
            api_hint: Some("openai-completions".to_string()),
            account: None,
        });
        assert!(matches!(client.route, Route::Completions(_)));
    }

    #[test]
    fn user_input_is_user_initiated_and_tool_follow_ups_are_agent() {
        assert_eq!(initiator(&[ChatMessage::user("hi")]), "user");
        let follow_up = [
            ChatMessage::user("run ls"),
            ChatMessage::assistant_blocks(vec![ChatContentBlock::tool_use(
                "call_1",
                "bash",
                json!({}),
            )]),
            ChatMessage::tool_results(vec![ToolResult {
                tool_use_id: "call_1".to_string(),
                content: ToolResultContent::Text("ok".to_string()),
                is_error: false,
            }]),
        ];
        assert_eq!(initiator(&follow_up), "agent");
        assert_eq!(initiator(&follow_up[..2]), "agent");
    }

    #[test]
    fn headers_carry_vscode_identity_and_vision_flag() {
        let headers = request_headers(&[ChatMessage::user("hi")]);
        assert_eq!(headers["user-agent"], "GitHubCopilotChat/0.35.0");
        assert_eq!(headers["editor-version"], "vscode/1.107.0");
        assert_eq!(headers["editor-plugin-version"], "copilot-chat/0.35.0");
        assert_eq!(headers["copilot-integration-id"], "vscode-chat");
        assert_eq!(headers["openai-intent"], "conversation-edits");
        assert_eq!(headers["x-initiator"], "user");
        assert!(headers.get("copilot-vision-request").is_none());

        let with_screenshot = [ChatMessage::tool_results(vec![ToolResult {
            tool_use_id: "call_1".to_string(),
            content: ToolResultContent::Blocks(vec![ToolResultBlock::Image {
                mime_type: "image/png".to_string(),
                data: "AAAA".to_string(),
            }]),
            is_error: false,
        }])];
        let headers = request_headers(&with_screenshot);
        assert_eq!(headers["copilot-vision-request"], "true");
        assert_eq!(headers["x-initiator"], "agent");
    }

    #[test]
    fn reasoning_is_omitted_when_off_and_for_non_gpt_models() {
        assert_eq!(
            responses_reasoning_effort(ThinkingLevel::Off, "gpt-6-luna"),
            None
        );
        assert_eq!(
            responses_reasoning_effort(ThinkingLevel::High, "grok-4.7"),
            None
        );
        assert_eq!(
            responses_reasoning_effort(ThinkingLevel::High, "gpt-6-luna").as_deref(),
            Some("high")
        );
    }

    #[test]
    fn copilot_is_an_oauth_subscription_provider() {
        let selection = crate::resolve_provider("copilot:claude-sonnet-4.6");
        assert_eq!(selection.kind, ProviderKind::GithubCopilot);
        assert_eq!(selection.model, "claude-sonnet-4.6");
        assert!(ProviderKind::GithubCopilot.supports_oauth());
        assert!(ProviderKind::GithubCopilot.is_subscription());
        assert_eq!(ProviderKind::GithubCopilot.api_key_env_var(), None);
    }
}
