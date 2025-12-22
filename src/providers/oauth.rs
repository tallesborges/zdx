//! OAuth token storage and retrieval.
//!
//! Stores OAuth tokens in `~/.zdx/oauth.json` with restricted permissions (0600).
//! Tokens are never logged or displayed in full.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::paths;

/// OAuth token cache filename.
const OAUTH_CACHE_FILE: &str = "oauth.json";

/// OAuth credentials for a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    /// Credential type (always "oauth" for OAuth tokens)
    #[serde(rename = "type")]
    pub cred_type: String,
    /// The refresh token (long-lived)
    pub refresh: String,
    /// The access token (short-lived)
    pub access: String,
    /// Expiry timestamp in milliseconds since epoch
    pub expires: u64,
}

impl OAuthCredentials {
    /// Returns true if the access token is expired or about to expire.
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        now >= self.expires
    }
}

/// OAuth token cache structure.
/// Maps provider names to their credentials.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OAuthCache {
    /// Provider name -> credentials mapping.
    #[serde(flatten)]
    pub providers: HashMap<String, OAuthCredentials>,
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

        let contents =
            serde_json::to_string_pretty(self).context("Failed to serialize OAuth cache")?;

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

    /// Gets the credentials for a provider.
    pub fn get(&self, provider: &str) -> Option<&OAuthCredentials> {
        self.providers.get(provider)
    }

    /// Sets the credentials for a provider.
    pub fn set(&mut self, provider: &str, creds: OAuthCredentials) {
        self.providers.insert(provider.to_string(), creds);
    }

    /// Removes the credentials for a provider.
    pub fn remove(&mut self, provider: &str) -> Option<OAuthCredentials> {
        self.providers.remove(provider)
    }
}

/// Anthropic-specific OAuth helpers.
pub mod anthropic {
    use super::*;
    use base64::prelude::*;
    use sha2::{Digest, Sha256};

    /// Provider key for Anthropic in the OAuth cache.
    pub const PROVIDER_KEY: &str = "anthropic";

    /// Anthropic OAuth client ID (base64 decoded at runtime)
    const CLIENT_ID_B64: &str = "OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl";

    /// Anthropic OAuth URLs
    const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
    const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
    const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
    const SCOPES: &str = "org:create_api_key user:profile user:inference";

    /// Get the decoded client ID
    fn client_id() -> String {
        String::from_utf8(BASE64_STANDARD.decode(CLIENT_ID_B64).unwrap_or_default())
            .unwrap_or_default()
    }

    /// PKCE code verifier and challenge
    pub struct Pkce {
        pub verifier: String,
        pub challenge: String,
    }

    /// Generate PKCE code verifier and challenge
    pub fn generate_pkce() -> Pkce {
        use rand::Rng;
        let mut rng = rand::rng();
        let verifier_bytes: [u8; 32] = rng.random();
        let verifier = BASE64_URL_SAFE_NO_PAD.encode(verifier_bytes);

        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = BASE64_URL_SAFE_NO_PAD.encode(hasher.finalize());

        Pkce {
            verifier,
            challenge,
        }
    }

    /// Build the authorization URL for Anthropic OAuth
    pub fn build_auth_url(pkce: &Pkce) -> String {
        let params = [
            ("code", "true"),
            ("client_id", &client_id()),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", &pkce.verifier),
        ];

        let query = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");

        format!("{}?{}", AUTHORIZE_URL, query)
    }

    /// Exchange authorization code for tokens
    pub async fn exchange_code(auth_code: &str, pkce: &Pkce) -> Result<OAuthCredentials> {
        // Parse auth code (format: code#state)
        let parts: Vec<&str> = auth_code.split('#').collect();
        if parts.len() != 2 {
            anyhow::bail!(
                "Invalid authorization code format. Expected 'code#state', got: {}",
                if auth_code.len() > 20 {
                    format!("{}...", &auth_code[..20])
                } else {
                    auth_code.to_string()
                }
            );
        }
        let code = parts[0];
        let state = parts[1];

        let client = reqwest::Client::new();
        let response = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "grant_type": "authorization_code",
                "client_id": client_id(),
                "code": code,
                "state": state,
                "redirect_uri": REDIRECT_URI,
                "code_verifier": pkce.verifier,
            }))
            .send()
            .await
            .context("Failed to send token exchange request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Token exchange failed (HTTP {}): {}", status, body);
        }

        let token_data: TokenResponse = response
            .json()
            .await
            .context("Failed to parse token response")?;

        // Calculate expiry time (current time + expires_in seconds - 5 min buffer)
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let expires_at = now + (token_data.expires_in * 1000) - (5 * 60 * 1000);

        Ok(OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: token_data.refresh_token,
            access: token_data.access_token,
            expires: expires_at,
        })
    }

    /// Refresh an expired access token
    pub async fn refresh_token(refresh_token: &str) -> Result<OAuthCredentials> {
        let client = reqwest::Client::new();
        let response = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "client_id": client_id(),
                "refresh_token": refresh_token,
            }))
            .send()
            .await
            .context("Failed to send token refresh request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed (HTTP {}): {}", status, body);
        }

        let token_data: TokenResponse = response
            .json()
            .await
            .context("Failed to parse token response")?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let expires_at = now + (token_data.expires_in * 1000) - (5 * 60 * 1000);

        Ok(OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: token_data.refresh_token,
            access: token_data.access_token,
            expires: expires_at,
        })
    }

    #[derive(Debug, Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: u64,
    }

    /// Loads the Anthropic OAuth credentials from cache.
    pub fn load_credentials() -> Result<Option<OAuthCredentials>> {
        let cache = OAuthCache::load()?;
        Ok(cache.get(PROVIDER_KEY).cloned())
    }

    /// Saves Anthropic OAuth credentials to cache.
    pub fn save_credentials(creds: &OAuthCredentials) -> Result<()> {
        let mut cache = OAuthCache::load()?;
        cache.set(PROVIDER_KEY, creds.clone());
        cache.save()?;
        Ok(())
    }

    /// Removes the Anthropic OAuth credentials from cache.
    pub fn clear_credentials() -> Result<bool> {
        let mut cache = OAuthCache::load()?;
        let had_creds = cache.remove(PROVIDER_KEY).is_some();
        cache.save()?;
        Ok(had_creds)
    }

    /// Get a valid access token, refreshing if necessary.
    pub async fn get_valid_token() -> Result<Option<String>> {
        let creds = match load_credentials()? {
            Some(c) => c,
            None => return Ok(None),
        };

        if creds.is_expired() {
            // Try to refresh
            match refresh_token(&creds.refresh).await {
                Ok(new_creds) => {
                    save_credentials(&new_creds)?;
                    Ok(Some(new_creds.access))
                }
                Err(e) => {
                    // Refresh failed, credentials are invalid
                    anyhow::bail!("OAuth token expired and refresh failed: {}. Please run `zdx login --anthropic` again.", e);
                }
            }
        } else {
            Ok(Some(creds.access))
        }
    }

    /// Returns a masked version of a token for display (first 12 chars + ...).
    pub fn mask_token(token: &str) -> String {
        if token.len() <= 16 {
            return "***".to_string();
        }
        format!("{}...", &token[..12])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: OAuthCredentials expiry check.
    #[test]
    fn test_credentials_expiry() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let expired = OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: "refresh".to_string(),
            access: "access".to_string(),
            expires: now - 1000, // 1 second ago
        };
        assert!(expired.is_expired());

        let valid = OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: "refresh".to_string(),
            access: "access".to_string(),
            expires: now + 60000, // 1 minute from now
        };
        assert!(!valid.is_expired());
    }

    /// Test: OAuthCache serialization roundtrip (in-memory, no fs).
    #[test]
    fn test_oauth_cache_serialization() {
        let mut cache = OAuthCache::default();
        cache.set(
            "anthropic",
            OAuthCredentials {
                cred_type: "oauth".to_string(),
                refresh: "refresh-token".to_string(),
                access: "access-token".to_string(),
                expires: 1234567890000,
            },
        );

        let json = serde_json::to_string(&cache).unwrap();
        let loaded: OAuthCache = serde_json::from_str(&json).unwrap();

        let creds = loaded.get("anthropic").unwrap();
        assert_eq!(creds.cred_type, "oauth");
        assert_eq!(creds.access, "access-token");
        assert_eq!(creds.refresh, "refresh-token");
    }

    /// Test: OAuthCache remove.
    #[test]
    fn test_oauth_cache_remove() {
        let mut cache = OAuthCache::default();
        cache.set(
            "anthropic",
            OAuthCredentials {
                cred_type: "oauth".to_string(),
                refresh: "r".to_string(),
                access: "a".to_string(),
                expires: 0,
            },
        );
        assert!(cache.get("anthropic").is_some());

        let removed = cache.remove("anthropic");
        assert!(removed.is_some());
        assert!(cache.get("anthropic").is_none());
    }

    /// Test: Token masking.
    #[test]
    fn test_mask_token() {
        assert_eq!(
            anthropic::mask_token("sk-ant-oat-long-token-here"),
            "sk-ant-oat-l..."
        );
        assert_eq!(anthropic::mask_token("short"), "***");
    }

    /// Test: PKCE generation produces valid output.
    #[test]
    fn test_pkce_generation() {
        let pkce = anthropic::generate_pkce();
        assert!(!pkce.verifier.is_empty());
        assert!(!pkce.challenge.is_empty());
        // Verifier should be base64url encoded 32 bytes = 43 chars
        assert!(pkce.verifier.len() >= 40);
    }

    /// Test: Auth URL contains required parameters.
    #[test]
    fn test_auth_url_format() {
        let pkce = anthropic::generate_pkce();
        let url = anthropic::build_auth_url(&pkce);

        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(url.contains("client_id="));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
    }
}
