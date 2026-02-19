//! Gemini CLI (Cloud Code Assist OAuth) provider.

use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};

use super::shared::{
    CloudCodeRequestParams, GeminiThinkingConfig, build_cloud_code_assist_request,
    classify_reqwest_error,
};
use super::sse::GeminiSseParser;
use crate::providers::debug_metrics::maybe_wrap_with_metrics;
use crate::providers::oauth::gemini_cli as oauth_gemini_cli;
use crate::providers::shared::merge_system_prompt;
use crate::providers::{ChatMessage, ProviderError, ProviderStream};
use crate::tools::ToolDefinition;

/// Cloud Code Assist API endpoint
const API_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com";

/// Stream generate content path
const STREAM_PATH: &str = "/v1internal:streamGenerateContent";

/// Runtime config for Gemini CLI requests.
#[derive(Debug, Clone)]
pub struct GeminiCliConfig {
    pub model: String,
    pub max_tokens: Option<u32>,
    /// Session ID for rate limit grouping (persists across requests in a session).
    pub session_id: String,
    /// Thinking configuration (level for Gemini 3, budget for Gemini 2.5)
    pub thinking_config: Option<GeminiThinkingConfig>,
}

impl GeminiCliConfig {
    pub fn new(
        model: String,
        max_tokens: Option<u32>,
        thinking_config: Option<GeminiThinkingConfig>,
    ) -> Self {
        Self {
            model,
            max_tokens,
            session_id: uuid::Uuid::new_v4().to_string(),
            thinking_config,
        }
    }
}

/// Resolves OAuth credentials, refreshing if expired.
///
/// # Errors
/// Returns an error if the operation fails.
pub async fn resolve_credentials() -> Result<oauth_gemini_cli::GeminiCliCredentials> {
    let mut creds = oauth_gemini_cli::load_credentials()?.ok_or_else(|| {
        anyhow::anyhow!(
            "No Gemini CLI OAuth credentials found. Run 'zdx login gemini-cli' to authenticate."
        )
    })?;

    let project_id = creds
        .account_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Missing project ID in credentials"))?;

    if creds.is_expired() {
        let refreshed = oauth_gemini_cli::refresh_token(&creds.refresh, &project_id)
            .await
            .context("Failed to refresh Gemini CLI OAuth token")?;
        oauth_gemini_cli::save_credentials(&refreshed)?;
        creds = refreshed;
    }

    Ok(oauth_gemini_cli::GeminiCliCredentials {
        access: creds.access,
        refresh: creds.refresh,
        expires: creds.expires,
        project_id,
    })
}

/// Gemini CLI client.
pub struct GeminiCliClient {
    config: GeminiCliConfig,
    http: reqwest::Client,
    /// Prompt sequence counter for `user_prompt_id` generation.
    prompt_seq: AtomicU32,
}

impl GeminiCliClient {
    pub fn new(config: GeminiCliConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            prompt_seq: AtomicU32::new(0),
        }
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
        let creds = resolve_credentials().await?;
        let seq = self.prompt_seq.fetch_add(1, Ordering::Relaxed);
        let system_prompt = merge_system_prompt(system);

        let request = build_cloud_code_assist_request(
            messages,
            tools,
            system_prompt.as_deref(),
            &CloudCodeRequestParams {
                model: &self.config.model,
                project_id: &creds.project_id,
                max_output_tokens: self.config.max_tokens,
                session_id: &self.config.session_id,
                prompt_seq: seq,
                thinking_config: self.config.thinking_config.as_ref(),
            },
        )?;

        let url = format!("{API_ENDPOINT}{STREAM_PATH}?alt=sse");
        let headers = build_headers(&creds.access);

        let response = self
            .http
            .post(&url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .map_err(|e| classify_reqwest_error(&e))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::http_status(status.as_u16(), &error_body).into());
        }

        let byte_stream = response.bytes_stream();
        let event_stream =
            GeminiSseParser::new(byte_stream, self.config.model.clone(), "gemini-cli");
        Ok(maybe_wrap_with_metrics(event_stream))
    }
}

fn build_headers(access_token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {access_token}"))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    // Mimic official Gemini CLI User-Agent for better rate limits
    headers.insert(
        "User-Agent",
        HeaderValue::from_static("GeminiCLI/0.23.0 (darwin; arm64)"),
    );
    headers.insert(
        "x-goog-api-client",
        HeaderValue::from_static("gl-node/22.16.0"),
    );
    headers.insert("Accept", HeaderValue::from_static("*/*"));
    headers.insert("Content-Type", HeaderValue::from_static("application/json"));
    headers
}
