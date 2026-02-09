//! `OpenAI` Codex (`ChatGPT` OAuth) provider.

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::header::{HeaderMap, HeaderValue};

use crate::prompts::CODEX_PROMPT_TEMPLATE;
use crate::providers::ProviderStream;
use crate::providers::oauth::openai_codex as oauth_codex;
use crate::providers::openai::responses::{ResponsesConfig, send_responses_stream};
use crate::tools::ToolDefinition;

const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";
const RESPONSES_PATH: &str = "/codex/responses";
const DEFAULT_TEXT_VERBOSITY: &str = "medium";

const HEADER_ACCOUNT_ID: &str = "chatgpt-account-id";
const HEADER_ORIGINATOR: &str = "originator";
const HEADER_USER_AGENT: &str = "user-agent";
const HEADER_SESSION_ID: &str = "session_id";

const ORIGINATOR_VALUE: &str = "zdx";
const USER_AGENT_VALUE: &str = concat!("zdx/", env!("CARGO_PKG_VERSION"));

/// Runtime config for `OpenAI` Codex requests.
#[derive(Debug, Clone)]
pub struct OpenAICodexConfig {
    pub model: String,
    #[allow(dead_code)]
    pub max_tokens: u32,
    pub reasoning_effort: Option<String>,
    pub prompt_cache_key: Option<String>,
}

impl OpenAICodexConfig {
    pub fn new(
        model: String,
        max_tokens: u32,
        reasoning_effort: Option<String>,
        prompt_cache_key: Option<String>,
    ) -> Self {
        Self {
            model,
            max_tokens,
            reasoning_effort,
            prompt_cache_key,
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
}

impl OpenAICodexClient {
    pub fn new(config: OpenAICodexConfig) -> Self {
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
        messages: &[crate::providers::ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let creds = resolve_credentials().await?;

        let headers = build_headers(
            &creds.account_id,
            &creds.access,
            self.config.prompt_cache_key.as_deref(),
        );
        let config = ResponsesConfig {
            base_url: DEFAULT_BASE_URL.to_string(),
            path: RESPONSES_PATH.to_string(),
            model: self.config.model.clone(),
            max_output_tokens: None,
            reasoning_effort: self.config.reasoning_effort.clone(),
            instructions: Some(CODEX_PROMPT_TEMPLATE.to_string()),
            text_verbosity: Some(DEFAULT_TEXT_VERBOSITY.to_string()),
            store: Some(false),
            include: Some(vec!["reasoning.encrypted_content".to_string()]),
            stream_options: None,
            prompt_cache_key: self.config.prompt_cache_key.clone(),
            parallel_tool_calls: Some(true),
            tool_choice: Some("auto".to_string()),
            truncation: None, // Default: "disabled" - fail if context exceeded
        };

        send_responses_stream(&self.http, &config, headers, messages, tools, system).await
    }
}

fn build_headers(account_id: &str, access_token: &str, session_id: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {access_token}"))
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    headers.insert(
        HEADER_ACCOUNT_ID,
        HeaderValue::from_str(account_id).unwrap_or_else(|_| HeaderValue::from_static("")),
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
    headers
}
