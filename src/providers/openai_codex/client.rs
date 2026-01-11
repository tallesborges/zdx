use std::pin::Pin;

use anyhow::Result;
use futures_util::Stream;
use reqwest::header::{HeaderMap, HeaderValue};

use crate::providers::StreamEvent;
use crate::providers::openai_codex::auth::{OpenAICodexConfig, resolve_credentials};
use crate::providers::openai_codex::prompts::{get_codex_instructions, normalize_model};
use crate::providers::openai_responses::{ResponsesConfig, send_responses_stream};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";
const RESPONSES_PATH: &str = "/codex/responses";

const HEADER_BETA: &str = "OpenAI-Beta";
const HEADER_ACCOUNT_ID: &str = "chatgpt-account-id";
const HEADER_ORIGINATOR: &str = "originator";
const HEADER_SESSION_ID: &str = "session_id";
const HEADER_CONVERSATION_ID: &str = "conversation_id";

const BETA_VALUE: &str = "responses=experimental";
const ORIGINATOR_VALUE: &str = "codex_cli_rs";

pub struct OpenAICodexClient {
    config: OpenAICodexConfig,
    http: reqwest::Client,
}

impl OpenAICodexClient {
    pub fn new(config: OpenAICodexConfig) -> Self {
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
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let creds = resolve_credentials().await?;

        let normalized_model = normalize_model(&self.config.model);
        let instructions = Some(get_codex_instructions(&normalized_model));
        let headers = build_headers(
            &creds.account_id,
            &creds.access,
            self.config.prompt_cache_key.as_deref(),
            self.config.prompt_cache_key.as_deref(),
        );
        let config = ResponsesConfig {
            base_url: DEFAULT_BASE_URL.to_string(),
            path: RESPONSES_PATH.to_string(),
            model: normalized_model,
            max_output_tokens: None,
            reasoning_effort: self.config.reasoning_effort.clone(),
            instructions,
            store: Some(false),
            include: Some(vec!["reasoning.encrypted_content".to_string()]),
            prompt_cache_key: self.config.prompt_cache_key.clone(),
            parallel_tool_calls: Some(true),
            tool_choice: Some("auto".to_string()),
            truncation: None, // Default: "disabled" - fail if context exceeded
        };

        send_responses_stream(&self.http, &config, headers, messages, tools, system).await
    }
}

fn build_headers(
    account_id: &str,
    access_token: &str,
    conversation_id: Option<&str>,
    session_id: Option<&str>,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", access_token))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert(
        HEADER_ACCOUNT_ID,
        HeaderValue::from_str(account_id).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert(HEADER_BETA, HeaderValue::from_static(BETA_VALUE));
    headers.insert(
        HEADER_ORIGINATOR,
        HeaderValue::from_static(ORIGINATOR_VALUE),
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    if let Some(value) = conversation_id {
        if let Ok(header_value) = HeaderValue::from_str(value) {
            headers.insert(HEADER_CONVERSATION_ID, header_value);
        }
    }
    if let Some(value) = session_id {
        if let Ok(header_value) = HeaderValue::from_str(value) {
            headers.insert(HEADER_SESSION_ID, header_value);
        }
    }
    headers
}
