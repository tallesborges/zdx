//! Gemini API key provider (Generative Language API).

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};

use super::shared::{
    GeminiThinkingConfig, build_gemini_request, classify_reqwest_error, merge_gemini_system_prompt,
};
use super::sse::GeminiSseParser;
use crate::providers::debug_metrics::maybe_wrap_with_metrics;
use crate::providers::{ChatMessage, DebugTrace, ProviderError, ProviderStream, wrap_stream};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Gemini API configuration.
#[derive(Debug, Clone)]
pub struct GeminiConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_output_tokens: u32,
    /// Thinking configuration (level for Gemini 3, budget for Gemini 2.5)
    pub thinking_config: Option<GeminiThinkingConfig>,
}

impl GeminiConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `GEMINI_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `GEMINI_API_KEY` (fallback if not in config)
    /// - `GEMINI_BASE_URL` (optional)
    pub fn from_env(
        model: String,
        max_output_tokens: u32,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        thinking_config: Option<GeminiThinkingConfig>,
    ) -> Result<Self> {
        let api_key = resolve_api_key(config_api_key)?;
        let base_url = resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_output_tokens,
            thinking_config,
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
    ) -> Result<ProviderStream> {
        let system_prompt = merge_gemini_system_prompt(system);
        let request = build_gemini_request(
            messages,
            tools,
            system_prompt.as_deref(),
            self.config.max_output_tokens,
            self.config.thinking_config.as_ref(),
        )?;
        let trace = DebugTrace::from_env(&self.config.model, None);
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.config.base_url, self.config.model
        );
        let headers = build_headers(&self.config.api_key);

        let response = if let Some(trace) = &trace {
            let body = serde_json::to_vec(&request)?;
            trace.write_request(&body);
            self.http
                .post(&url)
                .headers(headers)
                .body(body)
                .send()
                .await
                .map_err(classify_reqwest_error)?
        } else {
            self.http
                .post(&url)
                .headers(headers)
                .json(&request)
                .send()
                .await
                .map_err(classify_reqwest_error)?
        };

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::http_status(status.as_u16(), &error_body).into());
        }

        let byte_stream = wrap_stream(trace, response.bytes_stream());
        let event_stream = GeminiSseParser::new(byte_stream, self.config.model.clone(), "gemini");
        Ok(maybe_wrap_with_metrics(event_stream))
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

/// Resolves API key with precedence: config > env.
fn resolve_api_key(config_api_key: Option<&str>) -> Result<String> {
    // Try config value first
    if let Some(key) = config_api_key {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    // Fall back to env var
    std::env::var("GEMINI_API_KEY")
        .context("No API key available. Set GEMINI_API_KEY or api_key in [providers.gemini].")
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
