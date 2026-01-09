//! Gemini CLI client for Cloud Code Assist API.

use std::pin::Pin;

use anyhow::Result;
use futures_util::Stream;
use reqwest::header::{HeaderMap, HeaderValue};

use crate::providers::gemini_cli::auth::{GeminiCliConfig, resolve_credentials};
use crate::providers::gemini_shared::sse::GeminiSseParser;
use crate::providers::gemini_shared::{build_cloud_code_assist_request, classify_reqwest_error};
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
}

impl GeminiCliClient {
    pub fn new(config: GeminiCliConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let creds = resolve_credentials().await?;

        let request = build_cloud_code_assist_request(
            messages,
            tools,
            system,
            &self.config.model,
            &creds.project_id,
            Some(self.config.max_tokens),
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
        Ok(Box::pin(event_stream))
    }
}

fn build_headers(access_token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", access_token))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert(
        "User-Agent",
        HeaderValue::from_static("google-cloud-sdk vscode_cloudshelleditor/0.1"),
    );
    headers.insert("X-Goog-Api-Client", HeaderValue::from_static("gl-rust/1.0"));
    headers.insert(
        "Client-Metadata",
        HeaderValue::from_static(
            r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#,
        ),
    );
    headers.insert("Accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("Content-Type", HeaderValue::from_static("application/json"));
    headers
}
