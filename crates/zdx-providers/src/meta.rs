//! Meta Model API provider (Responses API).
//!
//! Serves Meta's Muse Spark family. Chat Completions is deliberately not
//! used: Meta redacts `reasoning_content` to empty for external keys and
//! exposes no reasoning summaries there, so thinking would never display.
//! The Responses API requests a natural-language summary via
//! `reasoning: { effort, summary: "auto" }` (streamed as
//! `response.reasoning_summary_text.*` events, already handled by the shared
//! Responses SSE parser) and replays reasoning statelessly across multi-turn
//! tool loops via `include: ["reasoning.encrypted_content"]`.

use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue};
use zdx_types::ToolDefinition;

use crate::openai::responses::{ResponsesConfig, send_responses_stream};
use crate::shared::merge_system_prompt;
use crate::{ProviderKind, ProviderStream};

const RESPONSES_PATH: &str = "/responses";

/// Bare model id, with any `provider:` prefix stripped.
fn bare_model_id(model: &str) -> &str {
    model.rsplit(':').next().unwrap_or(model)
}

/// Whether this Muse Spark model accepts the `max` reasoning effort.
/// Per Meta docs, `max` is Standard-tier `muse-spark-1.3` only;
/// Contributor-tier models reject it.
fn supports_max_effort(model: &str) -> bool {
    bare_model_id(model) == "muse-spark-1.3"
}

/// Maps a ZDX thinking level to Meta's `reasoning_effort` vocabulary.
/// Returns `None` when thinking is off (Meta rejects explicit `"none"`
/// with HTTP 400, so the field is omitted and the model uses its default).
fn reasoning_effort_from_thinking_level(
    level: zdx_types::ThinkingLevel,
    model: &str,
) -> Option<&'static str> {
    match level {
        zdx_types::ThinkingLevel::Off => None,
        zdx_types::ThinkingLevel::Low => Some("low"),
        zdx_types::ThinkingLevel::Medium => Some("medium"),
        zdx_types::ThinkingLevel::High => Some("high"),
        zdx_types::ThinkingLevel::Max if supports_max_effort(model) => Some("max"),
        zdx_types::ThinkingLevel::XHigh | zdx_types::ThinkingLevel::Max => Some("xhigh"),
    }
}

/// Meta Model API configuration.
#[derive(Debug, Clone)]
pub struct MetaConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub prompt_cache_key: Option<String>,
    pub reasoning_effort: Option<String>,
}

impl MetaConfig {
    /// Creates a new config from environment/config.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `META_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `META_API_KEY` (fallback if not in config)
    /// - `META_API_BASE` (optional base URL override)
    ///
    /// # Errors
    /// Returns an error if the API key / base URL cannot be resolved.
    pub fn from_env(
        model: String,
        max_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        prompt_cache_key: Option<String>,
        reasoning_effort: Option<String>,
    ) -> Result<Self> {
        let api_key = ProviderKind::Meta.resolve_api_key(config_api_key)?;
        let base_url = ProviderKind::Meta.resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
            prompt_cache_key,
            reasoning_effort,
        })
    }
}

fn build_headers(api_key: &str) -> anyhow::Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        crate::shared::header_value("Meta API key", &format!("Bearer {api_key}"))?,
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "user-agent",
        HeaderValue::from_static(crate::shared::USER_AGENT),
    );
    Ok(headers)
}

/// Meta Model API client using the Responses API.
pub struct MetaClient {
    api_key: String,
    config: ResponsesConfig,
    http: reqwest::Client,
}

impl MetaClient {
    pub fn new(config: MetaConfig) -> Self {
        // A reasoning summary is what makes thinking visible: raw reasoning
        // stays private on Meta's API, so request the compact `auto` summary
        // whenever reasoning is enabled. Summaries are best-effort — Meta may
        // return an empty summary when the model barely reasons.
        let reasoning_summary = config.reasoning_effort.as_ref().map(|_| "auto".to_string());
        Self {
            api_key: config.api_key,
            config: ResponsesConfig {
                base_url: config.base_url,
                path: RESPONSES_PATH.to_string(),
                model: config.model,
                max_output_tokens: config.max_tokens,
                reasoning_effort: config.reasoning_effort,
                reasoning_summary,
                instructions: None,
                text_verbosity: None,
                store: Some(false),
                include: Some(vec!["reasoning.encrypted_content".to_string()]),
                stream_options: None,
                prompt_cache_key: config.prompt_cache_key,
                parallel_tool_calls: Some(true),
                tool_choice: Some("auto".to_string()),
                truncation: None,
                service_tier: None,
            },
            http: reqwest::Client::new(),
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
        send_responses_stream(
            "meta",
            &self.http,
            &self.config,
            build_headers(&self.api_key)?,
            messages,
            tools,
            system.as_deref(),
        )
        .await
    }
}

/// Constructs the Meta client from the given context.
///
/// # Errors
/// Returns an error if the API key / base URL cannot be resolved from env or config.
pub fn build(
    ctx: &crate::ProviderBuildContext<'_>,
) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    Ok(Box::new(MetaClient::new(MetaConfig::from_env(
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.base_url,
        ctx.api_key,
        ctx.cache_key.clone(),
        reasoning_effort_from_thinking_level(ctx.thinking_level, ctx.model).map(str::to_owned),
    )?)))
}

#[cfg(test)]
mod tests {
    use zdx_types::ThinkingLevel;

    use super::{MetaClient, MetaConfig, RESPONSES_PATH, reasoning_effort_from_thinking_level};
    use crate::{ChatMessage, MessageContent, ProviderKind, resolve_provider};

    fn test_config(model: &str, effort: Option<&str>) -> MetaConfig {
        MetaConfig {
            api_key: "test-key".to_string(),
            base_url: "https://api.meta.ai/v1".to_string(),
            model: model.to_string(),
            max_tokens: Some(1024),
            prompt_cache_key: Some("thread-123".to_string()),
            reasoning_effort: effort.map(str::to_owned),
        }
    }

    fn user_message(text: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            phase: None,
            context: None,
            context_key: None,
            content: MessageContent::Text(text.to_string()),
        }
    }

    #[test]
    fn meta_prefix_routes_to_meta_with_bare_model() {
        let selection = resolve_provider("meta:muse-spark-1.1");
        assert_eq!(selection.kind, ProviderKind::Meta);
        assert_eq!(selection.model, "muse-spark-1.1");
    }

    #[test]
    fn meta_id_resolves_kind() {
        assert_eq!(ProviderKind::from_id("meta"), Some(ProviderKind::Meta));
    }

    #[test]
    fn meta_metadata_matches_model_api() {
        assert_eq!(ProviderKind::Meta.id(), "meta");
        assert_eq!(
            ProviderKind::Meta.default_base_url(),
            "https://api.meta.ai/v1"
        );
        assert_eq!(ProviderKind::Meta.api_key_env_var(), Some("META_API_KEY"));
        assert_eq!(ProviderKind::Meta.base_url_env_var(), Some("META_API_BASE"));
        assert!(!ProviderKind::Meta.is_subscription());
        assert!(!ProviderKind::Meta.supports_oauth());
    }

    #[test]
    fn meta_reasoning_effort_maps_thinking_levels() {
        let model = "muse-spark-1.3";
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::Off, model),
            None
        );
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::Low, model),
            Some("low")
        );
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::Medium, model),
            Some("medium")
        );
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::High, model),
            Some("high")
        );
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::XHigh, model),
            Some("xhigh")
        );
    }

    #[test]
    fn meta_max_effort_is_standard_tier_only() {
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::Max, "muse-spark-1.3"),
            Some("max")
        );
        // Prefixed ids resolve to the same bare model.
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::Max, "meta:muse-spark-1.3"),
            Some("max")
        );
        // Contributor-tier models reject `max`; clamp to `xhigh`.
        for model in ["muse-spark-1.3-contributor", "muse-spark-1.1"] {
            assert_eq!(
                reasoning_effort_from_thinking_level(ThinkingLevel::Max, model),
                Some("xhigh"),
                "{model}"
            );
        }
    }

    #[test]
    fn meta_uses_responses_api_with_encrypted_reasoning() {
        let client = MetaClient::new(test_config("muse-spark-1.3", Some("high")));
        assert_eq!(client.config.path, RESPONSES_PATH);
        assert_eq!(client.config.model, "muse-spark-1.3");
        assert_eq!(client.config.store, Some(false));
        assert_eq!(
            client.config.include.as_ref(),
            Some(&vec!["reasoning.encrypted_content".to_string()])
        );
    }

    #[test]
    fn meta_requests_reasoning_summary_when_thinking_enabled() {
        let client = MetaClient::new(test_config("muse-spark-1.3", Some("high")));
        let body = crate::openai::responses::build_request_body(
            &client.config,
            &[user_message(
                "Prove that the square root of 2 is irrational.",
            )],
            &[],
            None,
            None,
        )
        .expect("request body should build");
        let value = serde_json::to_value(&body).expect("request body should serialize");
        assert_eq!(
            value
                .get("reasoning")
                .and_then(|r| r.get("effort"))
                .and_then(serde_json::Value::as_str),
            Some("high")
        );
        assert_eq!(
            value
                .get("reasoning")
                .and_then(|r| r.get("summary"))
                .and_then(serde_json::Value::as_str),
            Some("auto")
        );
    }

    #[test]
    fn meta_omits_reasoning_when_thinking_off() {
        let client = MetaClient::new(test_config("muse-spark-1.3", None));
        let body = crate::openai::responses::build_request_body(
            &client.config,
            &[user_message("Hello.")],
            &[],
            None,
            None,
        )
        .expect("request body should build");
        let value = serde_json::to_value(&body).expect("request body should serialize");
        assert!(
            value.get("reasoning").is_none(),
            "reasoning must be omitted when thinking is off, got: {value}"
        );
    }
}
