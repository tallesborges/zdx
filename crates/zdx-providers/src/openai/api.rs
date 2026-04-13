//! `OpenAI` API key provider (Responses API).

use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue};
use zdx_types::{TextVerbosity, ToolDefinition};

use crate::openai::responses::{ResponsesConfig, StreamOptions, send_responses_stream};
use crate::{ProviderKind, ProviderStream};

const RESPONSES_PATH: &str = "/responses";

/// `OpenAI` API configuration.
#[derive(Debug, Clone)]
pub struct OpenAIConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_output_tokens: Option<u32>,
    pub reasoning_effort: Option<String>,
    pub text_verbosity: Option<TextVerbosity>,
    pub prompt_cache_key: Option<String>,
}

impl OpenAIConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `OPENAI_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `OPENAI_API_KEY` (fallback if not in config)
    /// - `OPENAI_BASE_URL` (optional)
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn from_env(
        model: String,
        max_output_tokens: Option<u32>,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        reasoning_effort: Option<String>,
        text_verbosity: Option<TextVerbosity>,
        prompt_cache_key: Option<String>,
    ) -> Result<Self> {
        let api_key = ProviderKind::OpenAI.resolve_api_key(config_api_key)?;
        let base_url = ProviderKind::OpenAI.resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_output_tokens,
            reasoning_effort,
            text_verbosity,
            prompt_cache_key,
        })
    }
}

/// `OpenAI` API client.
pub struct OpenAIClient {
    config: OpenAIConfig,
    http: reqwest::Client,
}

impl OpenAIClient {
    pub fn new(config: OpenAIConfig) -> Self {
        Self {
            config,
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
        let headers = build_headers(&self.config.api_key);
        let config = ResponsesConfig {
            base_url: self.config.base_url.clone(),
            path: RESPONSES_PATH.to_string(),
            model: self.config.model.clone(),
            max_output_tokens: self.config.max_output_tokens,
            reasoning_effort: self.config.reasoning_effort.clone(),
            reasoning_summary: None,
            instructions: None,
            text_verbosity: Some(
                self.config
                    .text_verbosity
                    .unwrap_or_default()
                    .as_str()
                    .to_string(),
            ),
            store: Some(false),
            include: Some(vec!["reasoning.encrypted_content".to_string()]),
            stream_options: Some(StreamOptions {
                include_obfuscation: Some(false),
            }),
            prompt_cache_key: self.config.prompt_cache_key.clone(),
            parallel_tool_calls: Some(true),
            tool_choice: Some("auto".to_string()),
            truncation: None, // Default: "disabled" - fail if context exceeded
        };

        send_responses_stream(&self.http, &config, headers, messages, tools, system).await
    }
}

fn build_headers(api_key: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {api_key}"))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "user-agent",
        HeaderValue::from_static(crate::shared::USER_AGENT),
    );
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_config_defaults_text_verbosity_to_medium_when_unset() {
        let config = OpenAIConfig {
            api_key: "test-key".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-5.4".to_string(),
            max_output_tokens: None,
            reasoning_effort: None,
            text_verbosity: None,
            prompt_cache_key: None,
        };

        assert_eq!(
            config.text_verbosity.unwrap_or_default().as_str(),
            TextVerbosity::Medium.as_str()
        );
    }
}
