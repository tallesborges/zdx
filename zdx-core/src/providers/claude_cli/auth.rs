//! Authentication helpers for Claude CLI (Anthropic OAuth).

use anyhow::{Context, Result};

use crate::providers::oauth::claude_cli as oauth_claude_cli;

/// Runtime config for Claude CLI requests.
#[derive(Debug, Clone)]
pub struct ClaudeCliConfig {
    pub model: String,
    pub max_tokens: u32,
    pub base_url: String,
    /// Whether extended thinking is enabled
    pub thinking_enabled: bool,
    /// Token budget for thinking (only used when thinking_enabled = true)
    pub thinking_budget_tokens: u32,
}

impl ClaudeCliConfig {
    pub fn new(
        model: String,
        max_tokens: u32,
        base_url: Option<&str>,
        thinking_enabled: bool,
        thinking_budget_tokens: u32,
    ) -> Self {
        let base_url = base_url
            .unwrap_or(crate::providers::anthropic::DEFAULT_BASE_URL)
            .to_string();
        Self {
            model,
            max_tokens,
            base_url,
            thinking_enabled,
            thinking_budget_tokens,
        }
    }
}

/// Resolves OAuth credentials, refreshing if expired.
pub async fn resolve_credentials() -> Result<oauth_claude_cli::ClaudeCliCredentials> {
    let mut creds = oauth_claude_cli::load_credentials()?.ok_or_else(|| {
        anyhow::anyhow!(
            "No Claude CLI OAuth credentials found. Run 'zdx login --claude-cli' to authenticate."
        )
    })?;

    if creds.is_expired() {
        let refreshed = oauth_claude_cli::refresh_token(&creds.refresh)
            .await
            .context("Failed to refresh Claude CLI OAuth token")?;
        oauth_claude_cli::save_credentials(&refreshed)?;
        creds = refreshed;
    }

    Ok(oauth_claude_cli::ClaudeCliCredentials {
        access: creds.access,
        refresh: creds.refresh,
        expires: creds.expires,
    })
}
