use anyhow::{Context, Result};

/// Default base URL for the Anthropic API.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Configuration for the Anthropic client.
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    /// The authentication token (API key)
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    /// Whether extended thinking is enabled
    pub thinking_enabled: bool,
    /// Token budget for thinking (only used when thinking_enabled = true)
    pub thinking_budget_tokens: u32,
}

impl AnthropicConfig {
    /// Creates a new config from environment.
    ///
    /// Authentication resolution order:
    /// 1. `config_api_key` parameter (from config file)
    /// 2. `ANTHROPIC_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `ANTHROPIC_API_KEY`: API key (fallback if not in config)
    /// - `ANTHROPIC_BASE_URL`: Optional base URL override
    ///
    /// Base URL resolution order:
    /// 1. `ANTHROPIC_BASE_URL` env var (if set and non-empty)
    /// 2. `config_base_url` parameter (if Some and non-empty)
    /// 3. Default: `https://api.anthropic.com`
    pub fn from_env(
        model: String,
        max_tokens: u32,
        config_base_url: Option<&str>,
        config_api_key: Option<&str>,
        thinking_enabled: bool,
        thinking_budget_tokens: u32,
    ) -> Result<Self> {
        let api_key = resolve_api_key(config_api_key)?;

        // Resolution order: env > config > default
        let base_url = Self::resolve_base_url(config_base_url)?;

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
            thinking_enabled,
            thinking_budget_tokens,
        })
    }

    /// Resolves the base URL with precedence: env > config > default.
    /// Validates that the URL is well-formed.
    fn resolve_base_url(config_base_url: Option<&str>) -> Result<String> {
        // Try env var first
        if let Ok(env_url) = std::env::var("ANTHROPIC_BASE_URL") {
            let trimmed = env_url.trim();
            if !trimmed.is_empty() {
                Self::validate_url(trimmed)?;
                return Ok(trimmed.to_string());
            }
        }

        // Try config value
        if let Some(config_url) = config_base_url {
            let trimmed = config_url.trim();
            if !trimmed.is_empty() {
                Self::validate_url(trimmed)?;
                return Ok(trimmed.to_string());
            }
        }

        // Default
        Ok(DEFAULT_BASE_URL.to_string())
    }

    /// Validates that a URL is well-formed.
    fn validate_url(url: &str) -> Result<()> {
        url::Url::parse(url).with_context(|| format!("Invalid Anthropic base URL: {}", url))?;
        Ok(())
    }
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
    std::env::var("ANTHROPIC_API_KEY")
        .context("No API key available. Set ANTHROPIC_API_KEY or api_key in [providers.anthropic].")
}
