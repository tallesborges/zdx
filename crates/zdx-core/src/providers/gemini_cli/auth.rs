//! Authentication helpers for Gemini CLI (Cloud Code Assist OAuth).

use anyhow::{Context, Result};

use crate::providers::gemini_shared::GeminiThinkingConfig;
use crate::providers::oauth::gemini_cli as oauth_gemini_cli;

/// Runtime config for Gemini CLI requests.
#[derive(Debug, Clone)]
pub struct GeminiCliConfig {
    pub model: String,
    pub max_tokens: u32,
    /// Session ID for rate limit grouping (persists across requests in a session).
    pub session_id: String,
    /// Thinking configuration (level for Gemini 3, budget for Gemini 2.5)
    pub thinking_config: Option<GeminiThinkingConfig>,
}

impl GeminiCliConfig {
    pub fn new(
        model: String,
        max_tokens: u32,
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

/// Resolves OAuth credentials, refreshing if expired.
pub async fn resolve_credentials() -> Result<oauth_gemini_cli::GeminiCliCredentials> {
    let mut creds = oauth_gemini_cli::load_credentials()?.ok_or_else(|| {
        anyhow::anyhow!(
            "No Gemini CLI OAuth credentials found. Run 'zdx login gemini-cli' to authenticate."
        )
    })?;

    let project_id = creds
        .account_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Missing project ID in credentials"))?;

    if creds.is_expired() {
        let refreshed = oauth_gemini_cli::refresh_token(&creds.refresh, &project_id)
            .await
            .context("Failed to refresh Gemini CLI OAuth token")?;
        oauth_gemini_cli::save_credentials(&refreshed)?;
        creds = refreshed;
    }

    Ok(oauth_gemini_cli::GeminiCliCredentials {
        access: creds.access,
        refresh: creds.refresh,
        expires: creds.expires,
        project_id,
    })
}
