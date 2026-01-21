//! Gemini CLI client for Cloud Code Assist API.

use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Result;
use futures_util::Stream;
use reqwest::header::{HeaderMap, HeaderValue};

use crate::providers::debug_metrics::maybe_wrap_with_metrics;
use crate::providers::gemini_cli::auth::{GeminiCliConfig, resolve_credentials};
use crate::providers::gemini_shared::sse::GeminiSseParser;
use crate::providers::gemini_shared::{
    CloudCodeRequestParams, build_cloud_code_assist_request, classify_reqwest_error,
    merge_gemini_system_prompt,
};
use crate::providers::{ChatMessage, ProviderError, StreamEvent};
use crate::tools::ToolDefinition;

/// Cloud Code Assist API endpoint
const API_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com";

/// Stream generate content path
const STREAM_PATH: &str = "/v1internal:streamGenerateContent";

/// Gemini CLI client.
pub struct GeminiCliClient {
    config: GeminiCliConfig,
    http: reqwest::Client,
    /// Prompt sequence counter for user_prompt_id generation.
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

    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let creds = resolve_credentials().await?;
        let seq = self.prompt_seq.fetch_add(1, Ordering::Relaxed);
        let system_prompt = merge_gemini_system_prompt(system);

        let request = build_cloud_code_assist_request(
            messages,
            tools,
            system_prompt.as_deref(),
            CloudCodeRequestParams {
                model: &self.config.model,
                project_id: &creds.project_id,
                max_output_tokens: Some(self.config.max_tokens),
                session_id: &self.config.session_id,
                prompt_seq: seq,
                thinking_config: self.config.thinking_config.as_ref(),
            },
        )?;

        let url = format!("{}{}?alt=sse", API_ENDPOINT, STREAM_PATH);
        let headers = build_headers(&creds.access);

        let response = self
            .http
            .post(&url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .map_err(classify_reqwest_error)?;

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
        HeaderValue::from_str(&format!("Bearer {}", access_token))
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
