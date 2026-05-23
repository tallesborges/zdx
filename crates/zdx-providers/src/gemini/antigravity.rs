//! Google Antigravity (Cloud Code Assist sandbox OAuth) provider.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use zdx_types::ToolDefinition;

use super::shared::{
    CloudCodeRequestParams, GeminiThinkingConfig, build_cloud_code_assist_request,
    classify_reqwest_error,
};
use super::sse::GeminiSseParser;
use crate::debug_metrics::maybe_wrap_with_metrics;
use crate::oauth::google_antigravity as oauth_antigravity;
use crate::shared::merge_system_prompt;
use crate::{ChatMessage, ProviderError, ProviderStream};

const API_ENDPOINT: &str = "https://daily-cloudcode-pa.googleapis.com";
const STREAM_PATH: &str = "/v1internal:streamGenerateContent";

#[derive(Debug, Clone)]
pub struct AntigravityConfig {
    pub model: String,
    pub max_tokens: Option<u32>,
    pub session_id: String,
    pub thinking_config: Option<GeminiThinkingConfig>,
}

impl AntigravityConfig {
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

pub async fn resolve_credentials() -> Result<oauth_antigravity::AntigravityCredentials> {
    let mut creds = oauth_antigravity::load_credentials()?.ok_or_else(|| {
        anyhow::anyhow!(
            "No Google Antigravity OAuth credentials found. Run 'zdx login --antigravity' to authenticate."
        )
    })?;

    let project_id = creds
        .account_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Missing project ID in credentials"))?;

    if creds.is_expired() {
        let refreshed = oauth_antigravity::refresh_token(&creds.refresh, &project_id)
            .await
            .context("Failed to refresh Google Antigravity OAuth token")?;
        oauth_antigravity::save_credentials(&refreshed)?;
        creds = refreshed;
    }

    Ok(oauth_antigravity::AntigravityCredentials {
        access: creds.access,
        refresh: creds.refresh,
        expires: creds.expires,
        project_id,
    })
}

pub struct AntigravityClient {
    config: AntigravityConfig,
    http: reqwest::Client,
    prompt_seq: AtomicU32,
}

impl AntigravityClient {
    pub fn new(config: AntigravityConfig) -> Self {
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
        let now_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let request_id = format!("agent-{now_millis}-{seq}");

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
                include_thoughts: true,
                request_type: Some("agent"),
                user_agent: Some("antigravity"),
                request_id: Some(request_id),
            },
        );

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
            GeminiSseParser::new(byte_stream, self.config.model.clone(), "google-antigravity");
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
    headers.insert(
        "User-Agent",
        HeaderValue::from_static("antigravity/2.0.6 darwin/arm64"),
    );
    headers.insert(
        "X-Goog-Api-Client",
        HeaderValue::from_static("google-cloud-sdk vscode_cloudshelleditor/0.1"),
    );
    headers.insert(
        "Client-Metadata",
        HeaderValue::from_static(
            r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#,
        ),
    );
    headers.insert("Accept", HeaderValue::from_static("*/*"));
    headers.insert("Content-Type", HeaderValue::from_static("application/json"));
    headers
}
