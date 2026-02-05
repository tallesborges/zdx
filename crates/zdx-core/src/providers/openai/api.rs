//! OpenAI API key provider (Responses API).

use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue};

use crate::providers::ProviderStream;
use crate::providers::openai::responses::{ResponsesConfig, StreamOptions, send_responses_stream};
use crate::providers::shared::{resolve_api_key, resolve_base_url};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const RESPONSES_PATH: &str = "/responses";
const DEFAULT_TEXT_VERBOSITY: &str = "medium";

/// OpenAI API configuration.
#[derive(Debug, Clone)]
pub struct OpenAIConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_output_tokens: u32,
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
    pub fn from_env(
        model: String,
        max_output_tokens: u32,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        prompt_cache_key: Option<String>,
    ) -> Result<Self> {
        let api_key = resolve_api_key(config_api_key, "OPENAI_API_KEY", "openai")?;
        let base_url = resolve_base_url(
            config_base_url,
            "OPENAI_BASE_URL",
            DEFAULT_BASE_URL,
            "OpenAI",
        )?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_output_tokens,
            prompt_cache_key,
        })
    }
}

/// OpenAI API client.
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

    pub async fn send_messages_stream(
        &self,
        messages: &[crate::providers::ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let headers = build_headers(&self.config.api_key);
        let config = ResponsesConfig {
            base_url: self.config.base_url.clone(),
            path: RESPONSES_PATH.to_string(),
            model: self.config.model.clone(),
            max_output_tokens: Some(self.config.max_output_tokens),
            reasoning_effort: None,
            instructions: None,
            text_verbosity: Some(DEFAULT_TEXT_VERBOSITY.to_string()),
            store: Some(false),
            include: None,
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
        HeaderValue::from_str(&format!("Bearer {}", api_key))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "user-agent",
        HeaderValue::from_static(crate::providers::shared::USER_AGENT),
    );
    headers
}
