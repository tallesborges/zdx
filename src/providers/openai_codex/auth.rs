//! Authentication helpers for OpenAI Codex (ChatGPT OAuth).

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::providers::oauth::openai_codex as oauth_codex;

/// Runtime config for OpenAI Codex requests.
#[derive(Debug, Clone)]
pub struct OpenAICodexConfig {
    pub model: String,
    #[allow(dead_code)]
    pub max_tokens: u32,
    pub reasoning_effort: Option<String>,
}

impl OpenAICodexConfig {
    pub fn new(model: String, max_tokens: u32, reasoning_effort: Option<String>) -> Self {
        Self {
            model,
            max_tokens,
            reasoning_effort,
        }
    }
}

/// Resolves OAuth credentials, refreshing if expired.
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

    let account_id = match creds.account_id.clone() {
        Some(id) => id,
        None => {
            let id = decode_account_id(&creds.access)
                .ok_or_else(|| anyhow::anyhow!("Failed to extract account_id from token"))?;
            creds.account_id = Some(id.clone());
            oauth_codex::save_credentials(&creds)?;
            id
        }
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
        .map(|s| s.to_string())
}
