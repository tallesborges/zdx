use anyhow::{Context, Result};

/// Default base URL for the Anthropic API.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Authentication method for Anthropic API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    /// API key authentication (uses `x-api-key` header)
    ApiKey,
    /// OAuth token authentication (uses `Authorization: Bearer` header)
    OAuth,
}

/// Configuration for the Anthropic client.
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    /// The authentication token (API key or OAuth access token)
    pub auth_token: String,
    /// The method of authentication
    pub auth_method: AuthMethod,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    /// Whether extended thinking is enabled
    pub thinking_enabled: bool,
    /// Token budget for thinking (only used when thinking_enabled = true)
    pub thinking_budget_tokens: u32,
}

impl AnthropicConfig {
    /// Creates a new config from environment and OAuth cache.
    ///
    /// Authentication resolution order:
    /// 1. OAuth token from `<base>/oauth.json` (if present and valid)
    /// 2. `ANTHROPIC_API_KEY` environment variable
    ///
    /// Environment variables:
    /// - `ANTHROPIC_API_KEY`: API key (used if no OAuth token)
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
        thinking_enabled: bool,
        thinking_budget_tokens: u32,
    ) -> Result<Self> {
        let (auth_token, auth_method) = Self::resolve_auth()?;

        // Resolution order: env > config > default
        let base_url = Self::resolve_base_url(config_base_url)?;

        Ok(Self {
            auth_token,
            auth_method,
            base_url,
            model,
            max_tokens,
            thinking_enabled,
            thinking_budget_tokens,
        })
    }

    /// Resolves authentication credentials.
    /// Precedence: OAuth token > ANTHROPIC_API_KEY
    fn resolve_auth() -> Result<(String, AuthMethod)> {
        use crate::providers::oauth::anthropic as oauth_anthropic;

        // Try OAuth token first
        match oauth_anthropic::load_credentials() {
            Ok(Some(creds)) => {
                if creds.is_expired() {
                    // Token expired, try to refresh synchronously
                    // Note: This blocks, but is acceptable at startup
                    let rt = tokio::runtime::Handle::try_current();
                    let refreshed = if let Ok(handle) = rt {
                        // We're already in a tokio context, spawn blocking
                        tokio::task::block_in_place(|| {
                            handle.block_on(oauth_anthropic::refresh_token(&creds.refresh))
                        })
                    } else {
                        // Not in tokio context, create a small runtime
                        tokio::runtime::Runtime::new()
                            .context("create runtime for token refresh")?
                            .block_on(oauth_anthropic::refresh_token(&creds.refresh))
                    };

                    match refreshed {
                        Ok(new_creds) => {
                            oauth_anthropic::save_credentials(&new_creds)?;
                            return Ok((new_creds.access, AuthMethod::OAuth));
                        }
                        Err(e) => {
                            // Refresh failed, clear credentials and fall through to API key
                            let _ = oauth_anthropic::clear_credentials();
                            eprintln!(
                                "OAuth token expired and refresh failed: {}. Falling back to ANTHROPIC_API_KEY.",
                                e
                            );
                        }
                    }
                } else {
                    // Token is valid
                    return Ok((creds.access, AuthMethod::OAuth));
                }
            }
            Ok(None) => {
                // No OAuth credentials, fall through to API key
            }
            Err(e) => {
                // Error loading OAuth cache, log and fall through
                eprintln!(
                    "Warning: Failed to load OAuth cache: {}. Using ANTHROPIC_API_KEY.",
                    e
                );
            }
        }

        // Fall back to API key
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .context("No authentication available. Either run `zdx login --anthropic` or set ANTHROPIC_API_KEY environment variable.")?;

        Ok((api_key, AuthMethod::ApiKey))
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
