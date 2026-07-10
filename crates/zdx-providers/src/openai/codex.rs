//! `OpenAI` Codex (`ChatGPT` OAuth) provider.

use std::sync::Arc;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::header::{HeaderMap, HeaderValue};
use zdx_assets::IDENTITY_PROMPT_TEMPLATE;
use zdx_types::{TextVerbosity, ToolDefinition};

use super::image_generation::{
    OpenAIGenerateImageResponse, OpenAIImageGenerationOptions, build_image_generation_request,
    parse_image_generation_sse_response,
};
use crate::oauth::openai_codex as oauth_codex;
use crate::openai::responses::{ResponsesConfig, send_responses_stream};
use crate::openai::responses_ws::{OpenAIResponsesWsClient, WsHeaderFactory};
use crate::{ProviderKind, ProviderStream};

const RESPONSES_PATH: &str = "/codex/responses";

const HEADER_ACCOUNT_ID: &str = "chatgpt-account-id";
const HEADER_ORIGINATOR: &str = "originator";
const HEADER_USER_AGENT: &str = "user-agent";
const HEADER_SESSION_ID: &str = "session_id";
const HEADER_OPENAI_BETA: &str = "OpenAI-Beta";

const ORIGINATOR_VALUE: &str = "zdx";
const USER_AGENT_VALUE: &str = concat!("zdx/", env!("CARGO_PKG_VERSION"));
/// Opt-in beta flag the `ChatGPT` Codex backend requires for the Responses
/// WebSocket transport.
const WS_BETA_VALUE: &str = "responses_websockets=2026-02-06";

fn supports_reasoning_summary(model: &str) -> bool {
    // Current Codex backend rejects `reasoning.summary` for Spark tier.
    !model.eq_ignore_ascii_case("gpt-5.3-codex-spark")
}

/// Runtime config for `OpenAI` Codex requests.
#[derive(Debug, Clone)]
pub struct OpenAICodexConfig {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub text_verbosity: Option<TextVerbosity>,
    pub prompt_cache_key: Option<String>,
    pub service_tier: Option<String>,
    /// Use the persistent WebSocket transport for the Codex Responses endpoint.
    pub websocket: bool,
}

impl OpenAICodexConfig {
    pub fn new(
        model: String,
        reasoning_effort: Option<String>,
        text_verbosity: Option<TextVerbosity>,
        prompt_cache_key: Option<String>,
        service_tier: Option<String>,
        websocket: bool,
    ) -> Self {
        Self {
            model,
            reasoning_effort,
            text_verbosity,
            prompt_cache_key,
            service_tier,
            websocket,
        }
    }
}

/// Resolves OAuth credentials, refreshing if expired.
///
/// # Errors
/// Returns an error if the operation fails.
pub async fn resolve_credentials() -> Result<oauth_codex::OpenAICodexCredentials> {
    let mut creds = oauth_codex::load_credentials()?
        .ok_or_else(|| anyhow::anyhow!("No OpenAI Codex OAuth credentials found"))?;

    if creds.is_expired() {
        let refreshed = oauth_codex::refresh_token(&creds.refresh)
            .await
            .context("Failed to refresh OpenAI Codex OAuth token")?;
        oauth_codex::save_credentials(&refreshed)?;
        creds = refreshed;
    }

    let account_id = if let Some(id) = creds.account_id.clone() {
        id
    } else {
        let id = decode_account_id(&creds.access)
            .ok_or_else(|| anyhow::anyhow!("Failed to extract account_id from token"))?;
        creds.account_id = Some(id.clone());
        oauth_codex::save_credentials(&creds)?;
        id
    };

    Ok(oauth_codex::OpenAICodexCredentials {
        access: creds.access,
        refresh: creds.refresh,
        expires: creds.expires,
        account_id,
    })
}

fn decode_account_id(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = parts[1];
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let claim = json.get(oauth_codex::JWT_CLAIM_PATH)?;
    claim
        .get("chatgpt_account_id")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
}

pub struct OpenAICodexClient {
    config: OpenAICodexConfig,
    http: reqwest::Client,
    ws: Option<Box<OpenAIResponsesWsClient>>,
}

impl OpenAICodexClient {
    pub fn new(config: OpenAICodexConfig) -> Self {
        let ws = config.websocket.then(|| {
            Box::new(OpenAIResponsesWsClient::new(
                codex_ws_header_factory(config.prompt_cache_key.clone()),
                codex_responses_config(&config, effective_codex_instructions(None)),
                true,
            ))
        });
        Self {
            config,
            http: reqwest::Client::new(),
            ws,
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
        if let Some(ws) = &self.ws {
            return ws.send_messages_stream(messages, tools, system).await;
        }

        let creds = resolve_credentials().await?;
        let headers = build_headers(
            &creds.account_id,
            &creds.access,
            self.config.prompt_cache_key.as_deref(),
        )?;
        let config = codex_responses_config(&self.config, effective_codex_instructions(system));

        // For Codex, send the system prompt through top-level `instructions`.
        // Keep `system` input empty here to avoid duplication in `input`.
        send_responses_stream(&self.http, &config, headers, messages, tools, None).await
    }

    /// Generate image content using the hosted Responses API `image_generation` tool.
    ///
    /// The Responses API uses a mainline model for tool orchestration; when callers pass
    /// `gpt-image-2` as the selected model, use the current Codex-capable default model
    /// and force the hosted image generation tool.
    ///
    /// # Errors
    /// Returns an error if the request fails or the response cannot be parsed.
    pub async fn generate_images(
        &self,
        prompt: &str,
        options: &OpenAIImageGenerationOptions,
    ) -> Result<OpenAIGenerateImageResponse> {
        let creds = resolve_credentials().await?;
        let headers = build_headers(
            &creds.account_id,
            &creds.access,
            self.config.prompt_cache_key.as_deref(),
        )?;

        let request = build_image_generation_request(&self.config.model, prompt, options);
        let url = format!(
            "{}{}",
            ProviderKind::OpenAICodex.default_base_url(),
            RESPONSES_PATH
        );
        let response = self
            .http
            .post(url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .map_err(|e| crate::ProviderError::timeout(format!("Request failed: {e}")))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(crate::ProviderError::http_status(status.as_u16(), &body).into());
        }

        parse_image_generation_sse_response(&body)
    }
}

fn effective_codex_instructions(system: Option<&str>) -> Option<String> {
    system
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| Some(IDENTITY_PROMPT_TEMPLATE.trim().to_string()))
}

fn codex_responses_config(
    config: &OpenAICodexConfig,
    instructions: Option<String>,
) -> ResponsesConfig {
    ResponsesConfig {
        base_url: ProviderKind::OpenAICodex.default_base_url().to_string(),
        path: RESPONSES_PATH.to_string(),
        model: config.model.clone(),
        max_output_tokens: None,
        reasoning_effort: config.reasoning_effort.clone(),
        reasoning_summary: supports_reasoning_summary(&config.model).then(|| "auto".to_string()),
        instructions,
        text_verbosity: Some(
            config
                .text_verbosity
                .unwrap_or_default()
                .as_str()
                .to_string(),
        ),
        store: Some(false),
        include: Some(vec!["reasoning.encrypted_content".to_string()]),
        stream_options: None,
        prompt_cache_key: config.prompt_cache_key.clone(),
        parallel_tool_calls: Some(true),
        tool_choice: Some("auto".to_string()),
        truncation: None, // Default: "disabled" - fail if context exceeded
        service_tier: config.service_tier.clone(),
    }
}

/// Builds the async header factory for the Codex WebSocket handshake: OAuth
/// bearer + account/originator/user-agent headers + the `responses_websockets`
/// beta flag, resolving (and refreshing) credentials at connect time.
fn codex_ws_header_factory(session_id: Option<String>) -> WsHeaderFactory {
    Arc::new(move || {
        let session_id = session_id.clone();
        Box::pin(async move {
            let creds = resolve_credentials().await?;
            let mut headers = vec![
                (
                    "Authorization".to_string(),
                    format!("Bearer {}", creds.access),
                ),
                (HEADER_ACCOUNT_ID.to_string(), creds.account_id),
                (HEADER_ORIGINATOR.to_string(), ORIGINATOR_VALUE.to_string()),
                (HEADER_USER_AGENT.to_string(), USER_AGENT_VALUE.to_string()),
                (HEADER_OPENAI_BETA.to_string(), WS_BETA_VALUE.to_string()),
            ];
            if let Some(session_id) = session_id {
                headers.push((HEADER_SESSION_ID.to_string(), session_id));
            }
            Ok(headers)
        })
    })
}

fn build_headers(
    account_id: &str,
    access_token: &str,
    session_id: Option<&str>,
) -> anyhow::Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        crate::shared::header_value("Codex access token", &format!("Bearer {access_token}"))?,
    );
    headers.insert(
        HEADER_ACCOUNT_ID,
        crate::shared::header_value("Codex account id", account_id)?,
    );
    headers.insert(
        HEADER_ORIGINATOR,
        HeaderValue::from_static(ORIGINATOR_VALUE),
    );
    headers.insert(
        HEADER_USER_AGENT,
        HeaderValue::from_static(USER_AGENT_VALUE),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    if let Some(value) = session_id
        && let Ok(header_value) = HeaderValue::from_str(value)
    {
        headers.insert(HEADER_SESSION_ID, header_value);
    }
    Ok(headers)
}

/// Constructs the `OpenAI Codex` client from the given context.
///
/// # Errors
/// Never returns an error; construction is infallible.
pub fn build(
    ctx: &crate::ProviderBuildContext<'_>,
) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    Ok(Box::new(OpenAICodexClient::new(OpenAICodexConfig::new(
        ctx.model.to_string(),
        super::responses_reasoning_effort(ctx.thinking_level, ctx.model).map(str::to_owned),
        ctx.text_verbosity.or(ctx.provider_text_verbosity),
        ctx.cache_key.clone(),
        ctx.service_tier.clone(),
        ctx.websocket,
    ))))
}

#[cfg(test)]
mod tests {
    use zdx_assets::IDENTITY_PROMPT_TEMPLATE;
    use zdx_types::TextVerbosity;

    use super::{effective_codex_instructions, supports_reasoning_summary};

    #[test]
    fn spark_model_disables_reasoning_summary() {
        assert!(!supports_reasoning_summary("gpt-5.3-codex-spark"));
    }

    #[test]
    fn non_spark_models_keep_reasoning_summary() {
        assert!(supports_reasoning_summary("gpt-5.3-codex"));
    }

    #[test]
    fn codex_instructions_fall_back_to_identity_prompt() {
        assert_eq!(
            effective_codex_instructions(None).as_deref(),
            Some(IDENTITY_PROMPT_TEMPLATE.trim())
        );
        assert_eq!(
            effective_codex_instructions(Some("   ")).as_deref(),
            Some(IDENTITY_PROMPT_TEMPLATE.trim())
        );
    }

    #[test]
    fn codex_instructions_prefer_explicit_system_prompt() {
        assert_eq!(
            effective_codex_instructions(Some(" custom system ")).as_deref(),
            Some("custom system")
        );
    }

    #[test]
    fn codex_config_defaults_text_verbosity_to_medium_when_unset() {
        let config =
            super::OpenAICodexConfig::new("gpt-5.4".to_string(), None, None, None, None, false);

        assert_eq!(
            config.text_verbosity.unwrap_or_default().as_str(),
            TextVerbosity::Medium.as_str()
        );
    }
}
