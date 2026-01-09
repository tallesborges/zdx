//! Gemini provider (Google Generative Language API).

use std::pin::Pin;

use anyhow::{Context, Result};
use futures_util::Stream;
use reqwest::header::{HeaderMap, HeaderValue};

use crate::providers::gemini_shared::sse::GeminiSseParser;
use crate::providers::gemini_shared::{build_gemini_request, classify_reqwest_error};
use crate::providers::{ChatMessage, ProviderError, StreamEvent};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Gemini API configuration.
#[derive(Debug, Clone)]
pub struct GeminiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_output_tokens: u32,
}

impl GeminiConfig {
    /// Creates a new config from environment.
    ///
    /// Environment variables:
    /// - `GEMINI_API_KEY` (required)
    /// - `GEMINI_BASE_URL` (optional)
    pub fn from_env(
        model: String,
        max_output_tokens: u32,
        config_base_url: Option<&str>,
    ) -> Result<Self> {
        let api_key = std::env::var("GEMINI_API_KEY")
            .context("GEMINI_API_KEY is not set. Set it to use Gemini.")?;
        let base_url = resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_output_tokens,
        })
    }
}

/// Gemini client.
pub struct GeminiClient {
    config: GeminiConfig,
    http: reqwest::Client,
}

impl GeminiClient {
    pub fn new(config: GeminiConfig) -> Self {
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
        let request = build_gemini_request(messages, tools, system, self.config.max_output_tokens)?;
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.config.base_url, self.config.model
        );
        let headers = build_headers(&self.config.api_key);

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
        let event_stream = GeminiSseParser::new(byte_stream, self.config.model.clone(), "gemini");
        Ok(Box::pin(event_stream))
    }
}

fn resolve_base_url(config_base_url: Option<&str>) -> Result<String> {
    if let Ok(env_url) = std::env::var("GEMINI_BASE_URL") {
        let trimmed = env_url.trim();
        if !trimmed.is_empty() {
            validate_url(trimmed)?;
            return Ok(trimmed.to_string());
        }
    }

    if let Some(config_url) = config_base_url {
        let trimmed = config_url.trim();
        if !trimmed.is_empty() {
            validate_url(trimmed)?;
            return Ok(trimmed.to_string());
        }
    }

    Ok(DEFAULT_BASE_URL.to_string())
}

fn validate_url(url: &str) -> Result<()> {
    url::Url::parse(url).with_context(|| format!("Invalid Gemini base URL: {}", url))?;
    Ok(())
}

fn build_headers(api_key: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-goog-api-key",
        HeaderValue::from_str(api_key).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers
}
