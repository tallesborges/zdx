//! OAuth token storage and retrieval.
//!
//! Stores OAuth tokens in `~/.zdx/oauth.json` with restricted permissions (0600).
//! Tokens are never logged or displayed in full.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::paths;

/// OAuth token cache filename.
const OAUTH_CACHE_FILE: &str = "oauth.json";

/// Anthropic OAuth token prefix.
pub const ANTHROPIC_OAUTH_PREFIX: &str = "sk-ant-oat";

/// OAuth token entry for a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    /// The access token.
    pub access_token: String,
    /// Optional refresh token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Optional expiry timestamp (Unix seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

impl OAuthToken {
    /// Creates a new OAuth token with just an access token.
    pub fn new(access_token: String) -> Self {
        Self {
            access_token,
            refresh_token: None,
            expires_at: None,
        }
    }
}

/// OAuth token cache structure.
/// Maps provider names to their tokens.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OAuthCache {
    /// Provider name -> token mapping.
    #[serde(flatten)]
    pub providers: HashMap<String, OAuthToken>,
}

impl OAuthCache {
    /// Returns the path to the OAuth cache file.
    pub fn cache_path() -> PathBuf {
        paths::zdx_home().join(OAUTH_CACHE_FILE)
    }

    /// Loads the OAuth cache from disk.
    /// Returns an empty cache if the file doesn't exist.
    pub fn load() -> Result<Self> {
        let path = Self::cache_path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read OAuth cache from {}", path.display()))?;

        serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse OAuth cache from {}", path.display()))
    }

    /// Saves the OAuth cache to disk with restricted permissions (0600).
    pub fn save(&self) -> Result<()> {
        let path = Self::cache_path();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }

        let contents = serde_json::to_string_pretty(self)
            .context("Failed to serialize OAuth cache")?;

        // Write with restricted permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .with_context(|| format!("Failed to open {} for writing", path.display()))?;
            file.write_all(contents.as_bytes())
                .with_context(|| format!("Failed to write to {}", path.display()))?;
        }

        #[cfg(not(unix))]
        {
            fs::write(&path, contents)
                .with_context(|| format!("Failed to write to {}", path.display()))?;
        }

        Ok(())
    }

    /// Gets the token for a provider.
    pub fn get(&self, provider: &str) -> Option<&OAuthToken> {
        self.providers.get(provider)
    }

    /// Sets the token for a provider.
    pub fn set(&mut self, provider: &str, token: OAuthToken) {
        self.providers.insert(provider.to_string(), token);
    }

    /// Removes the token for a provider.
    pub fn remove(&mut self, provider: &str) -> Option<OAuthToken> {
        self.providers.remove(provider)
    }

    /// Returns true if the cache has a token for the given provider.
    pub fn has(&self, provider: &str) -> bool {
        self.providers.contains_key(provider)
    }
}

/// Anthropic-specific OAuth helpers.
pub mod anthropic {
    use super::*;

    /// Provider key for Anthropic in the OAuth cache.
    pub const PROVIDER_KEY: &str = "anthropic";

    /// Anthropic OAuth authorization URL.
    /// Users visit this URL to authorize and get a token.
    pub const AUTH_URL: &str = "https://console.anthropic.com/settings/keys";

    /// Loads the Anthropic OAuth token from cache.
    pub fn load_token() -> Result<Option<String>> {
        let cache = OAuthCache::load()?;
        Ok(cache.get(PROVIDER_KEY).map(|t| t.access_token.clone()))
    }

    /// Saves an Anthropic OAuth token to cache.
    pub fn save_token(token: &str) -> Result<()> {
        let mut cache = OAuthCache::load()?;
        cache.set(PROVIDER_KEY, OAuthToken::new(token.to_string()));
        cache.save()?;
        Ok(())
    }

    /// Removes the Anthropic OAuth token from cache.
    pub fn clear_token() -> Result<bool> {
        let mut cache = OAuthCache::load()?;
        let had_token = cache.remove(PROVIDER_KEY).is_some();
        cache.save()?;
        Ok(had_token)
    }

    /// Returns true if the token looks like an Anthropic OAuth token.
    pub fn is_oauth_token(token: &str) -> bool {
        token.starts_with(ANTHROPIC_OAUTH_PREFIX)
    }

    /// Validates a token format (basic checks).
    /// Returns Ok(()) if valid, Err with message if invalid.
    pub fn validate_token_format(token: &str) -> Result<()> {
        let token = token.trim();
        if token.is_empty() {
            anyhow::bail!("Token cannot be empty");
        }
        // Basic length check (Anthropic tokens are typically 100+ chars)
        if token.len() < 20 {
            anyhow::bail!("Token appears too short");
        }
        // Check for common paste errors (newlines, spaces)
        if token.contains('\n') || token.contains('\r') {
            anyhow::bail!("Token contains newlines - please paste the token without line breaks");
        }
        Ok(())
    }

    /// Returns a masked version of a token for display (first 8 chars + ...).
    pub fn mask_token(token: &str) -> String {
        if token.len() <= 12 {
            return "***".to_string();
        }
        format!("{}...", &token[..12])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: Token format validation.
    #[test]
    fn test_validate_token_format() {
        assert!(anthropic::validate_token_format("sk-ant-oat-valid-token-12345678901234567890").is_ok());
        assert!(anthropic::validate_token_format("").is_err());
        assert!(anthropic::validate_token_format("short").is_err());
        assert!(anthropic::validate_token_format("token\nwith\nnewlines").is_err());
    }

    /// Test: OAuth token prefix detection.
    #[test]
    fn test_is_oauth_token() {
        assert!(anthropic::is_oauth_token("sk-ant-oat-something"));
        assert!(!anthropic::is_oauth_token("sk-ant-api-something"));
    }

    /// Test: Token masking.
    #[test]
    fn test_mask_token() {
        assert_eq!(anthropic::mask_token("sk-ant-oat-long-token"), "sk-ant-oat-l...");
        assert_eq!(anthropic::mask_token("short"), "***");
    }

    /// Test: OAuthCache serialization roundtrip (in-memory, no fs).
    #[test]
    fn test_oauth_cache_serialization() {
        let mut cache = OAuthCache::default();
        cache.set("anthropic", OAuthToken::new("sk-test-token".to_string()));

        let json = serde_json::to_string(&cache).unwrap();
        let loaded: OAuthCache = serde_json::from_str(&json).unwrap();

        assert!(loaded.has("anthropic"));
        assert_eq!(loaded.get("anthropic").unwrap().access_token, "sk-test-token");
    }

    /// Test: OAuthCache remove.
    #[test]
    fn test_oauth_cache_remove() {
        let mut cache = OAuthCache::default();
        cache.set("anthropic", OAuthToken::new("sk-test-token".to_string()));
        assert!(cache.has("anthropic"));

        let removed = cache.remove("anthropic");
        assert!(removed.is_some());
        assert!(!cache.has("anthropic"));

        let removed_again = cache.remove("anthropic");
        assert!(removed_again.is_none());
    }
}
