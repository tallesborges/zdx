//! OAuth token storage and retrieval.
//!
//! Stores OAuth tokens in `<base>/oauth.json` with restricted permissions (0600).
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

fn now_millis_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(u64::MAX)
}

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
    /// Optional account identifier for providers that require it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl OAuthCredentials {
    /// Returns true if the access token is expired or about to expire.
    pub fn is_expired(&self) -> bool {
        let now = now_millis_u64();
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
    ///
    /// # Errors
    /// Returns an error if the operation fails.
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
    ///
    /// # Errors
    /// Returns an error if the operation fails.
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

/// Claude CLI (Anthropic OAuth) helpers.
pub mod claude_cli {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use sha2::{Digest, Sha256};

    use super::{Context, Deserialize, OAuthCache, OAuthCredentials, Result};

    /// Provider key for Claude CLI in the OAuth cache.
    pub const PROVIDER_KEY: &str = "claude-cli";

    /// Anthropic OAuth client ID (public, not a secret)
    const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

    /// Anthropic OAuth URLs
    const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
    const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
    /// Local OAuth callback path (port is dynamic).
    pub const LOCAL_CALLBACK_PATH: &str = "/callback";
    const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code";
    const CLIENT_HINT: &str = "claude-code";

    /// PKCE code verifier and challenge
    pub struct Pkce {
        pub verifier: String,
        pub challenge: String,
    }

    /// Claude CLI credentials with expiry.
    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct ClaudeCliCredentials {
        pub access: String,
        pub refresh: String,
        pub expires: u64,
    }

    /// Generate PKCE code verifier and challenge
    pub fn generate_pkce() -> Pkce {
        // Use two UUIDs (16 bytes each) to get 32 random bytes
        let uuid1 = uuid::Uuid::new_v4();
        let uuid2 = uuid::Uuid::new_v4();
        let mut verifier_bytes = [0u8; 32];
        verifier_bytes[..16].copy_from_slice(uuid1.as_bytes());
        verifier_bytes[16..].copy_from_slice(uuid2.as_bytes());
        let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

        Pkce {
            verifier,
            challenge,
        }
    }

    /// Build the authorization URL for Claude CLI OAuth
    pub fn build_auth_url(pkce: &Pkce, state: &str, redirect_uri: &str) -> String {
        let params = [
            ("code", "true"),
            ("client_id", CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri),
            ("scope", SCOPES),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("client", CLIENT_HINT),
        ];

        let query: String = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(params)
            .finish();

        format!("{AUTHORIZE_URL}?{query}")
    }

    /// Builds the redirect URI for a given localhost port.
    pub fn build_redirect_uri(port: u16) -> String {
        format!("http://localhost:{port}{LOCAL_CALLBACK_PATH}")
    }

    /// Generates a random high localhost port for OAuth callbacks.
    pub fn random_local_port() -> u16 {
        let id = uuid::Uuid::new_v4();
        let bytes = id.as_bytes();
        let raw = u16::from_le_bytes([bytes[0], bytes[1]]);
        49152 + (raw % 16384)
    }

    /// Parses a pasted authorization input into code + optional state.
    pub fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
        let value = input.trim();
        if value.is_empty() {
            return (None, None);
        }

        if let Ok(url) = url::Url::parse(value) {
            let code = url.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v);
            let state = url
                .query_pairs()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v);
            return (code.map(|v| v.to_string()), state.map(|v| v.to_string()));
        }

        if let Some((code, state)) = value.split_once('#') {
            return (Some(code.to_string()), Some(state.to_string()));
        }

        if value.contains("code=") {
            let params = url::form_urlencoded::parse(value.as_bytes()).collect::<Vec<_>>();
            let code = params.iter().find(|(k, _)| k == "code").map(|(_, v)| v);
            let state = params.iter().find(|(k, _)| k == "state").map(|(_, v)| v);
            return (
                code.map(std::string::ToString::to_string),
                state.map(std::string::ToString::to_string),
            );
        }

        (Some(value.to_string()), None)
    }

    /// Exchange authorization code for tokens
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn exchange_code(
        auth_code: &str,
        pkce: &Pkce,
        redirect_uri: &str,
    ) -> Result<OAuthCredentials> {
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
                "client_id": CLIENT_ID,
                "code": code,
                "state": state,
                "redirect_uri": redirect_uri,
                "code_verifier": pkce.verifier,
            }))
            .send()
            .await
            .context("Failed to send token exchange request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Token exchange failed (HTTP {status}): {body}");
        }

        let token_data: TokenResponse = response
            .json()
            .await
            .context("Failed to parse token response")?;

        // Calculate expiry time (current time + expires_in seconds - 5 min buffer)
        let now = super::now_millis_u64();
        let expires_at = now + (token_data.expires_in * 1000) - (5 * 60 * 1000);

        Ok(OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: token_data.refresh_token,
            access: token_data.access_token,
            expires: expires_at,
            account_id: None,
        })
    }

    /// Refresh an expired access token
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn refresh_token(refresh_token: &str) -> Result<OAuthCredentials> {
        let client = reqwest::Client::new();
        let response = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "client_id": CLIENT_ID,
                "refresh_token": refresh_token,
            }))
            .send()
            .await
            .context("Failed to send token refresh request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed (HTTP {status}): {body}");
        }

        let token_data: TokenResponse = response
            .json()
            .await
            .context("Failed to parse token response")?;

        let now = super::now_millis_u64();
        let expires_at = now + (token_data.expires_in * 1000) - (5 * 60 * 1000);

        Ok(OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: token_data.refresh_token,
            access: token_data.access_token,
            expires: expires_at,
            account_id: None,
        })
    }

    #[derive(Debug, Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: u64,
    }

    /// Loads the Claude CLI OAuth credentials from cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn load_credentials() -> Result<Option<OAuthCredentials>> {
        let cache = OAuthCache::load()?;
        Ok(cache.get(PROVIDER_KEY).cloned())
    }

    /// Saves Claude CLI OAuth credentials to cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_credentials(creds: &OAuthCredentials) -> Result<()> {
        let mut cache = OAuthCache::load()?;
        cache.set(PROVIDER_KEY, creds.clone());
        cache.save()?;
        Ok(())
    }

    /// Removes the Claude CLI OAuth credentials from cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn clear_credentials() -> Result<bool> {
        let mut cache = OAuthCache::load()?;
        let had_creds = cache.remove(PROVIDER_KEY).is_some();
        cache.save()?;
        Ok(had_creds)
    }

    /// Returns a masked version of a token for display (first 12 chars + ...).
    pub fn mask_token(token: &str) -> String {
        if token.len() <= 16 {
            return "***".to_string();
        }
        format!("{}...", &token[..12])
    }
}

/// `OpenAI` Codex (`ChatGPT` OAuth) helpers.
pub mod openai_codex {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use sha2::{Digest, Sha256};

    use super::{Context, Deserialize, OAuthCache, OAuthCredentials, Result};

    /// Provider key for `OpenAI` Codex in the OAuth cache.
    pub const PROVIDER_KEY: &str = "openai-codex";

    /// `OpenAI` Codex OAuth client ID (public, not a secret)
    const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

    /// `OpenAI` OAuth URLs
    const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
    const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
    const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
    const SCOPES: &str = "openid profile email offline_access";

    /// JWT claim path used to extract the `ChatGPT` account id.
    pub const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

    /// PKCE code verifier and challenge
    pub struct Pkce {
        pub verifier: String,
        pub challenge: String,
    }

    /// `OpenAI` Codex credentials with account id.
    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct OpenAICodexCredentials {
        pub access: String,
        pub refresh: String,
        pub expires: u64,
        pub account_id: String,
    }

    /// Generate PKCE code verifier and challenge
    pub fn generate_pkce() -> Pkce {
        let uuid1 = uuid::Uuid::new_v4();
        let uuid2 = uuid::Uuid::new_v4();
        let mut verifier_bytes = [0u8; 32];
        verifier_bytes[..16].copy_from_slice(uuid1.as_bytes());
        verifier_bytes[16..].copy_from_slice(uuid2.as_bytes());
        let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

        Pkce {
            verifier,
            challenge,
        }
    }

    /// Build the authorization URL for `OpenAI` Codex OAuth
    pub fn build_auth_url(pkce: &Pkce, state: &str) -> String {
        let params = [
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "codex_cli_rs"),
        ];

        let query: String = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(params)
            .finish();

        format!("{AUTHORIZE_URL}?{query}")
    }

    /// Parses a pasted authorization input into code + optional state.
    pub fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
        let value = input.trim();
        if value.is_empty() {
            return (None, None);
        }

        if let Ok(url) = url::Url::parse(value) {
            let code = url.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v);
            let state = url
                .query_pairs()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v);
            return (code.map(|v| v.to_string()), state.map(|v| v.to_string()));
        }

        if let Some((code, state)) = value.split_once('#') {
            return (Some(code.to_string()), Some(state.to_string()));
        }

        if value.contains("code=") {
            let params = url::form_urlencoded::parse(value.as_bytes()).collect::<Vec<_>>();
            let code = params.iter().find(|(k, _)| k == "code").map(|(_, v)| v);
            let state = params.iter().find(|(k, _)| k == "state").map(|(_, v)| v);
            return (
                code.map(std::string::ToString::to_string),
                state.map(std::string::ToString::to_string),
            );
        }

        (Some(value.to_string()), None)
    }

    /// Exchanges authorization code for tokens.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn exchange_code(auth_code: &str, pkce: &Pkce) -> Result<OAuthCredentials> {
        let client = reqwest::Client::new();
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "authorization_code")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("code", auth_code)
            .append_pair("code_verifier", &pkce.verifier)
            .append_pair("redirect_uri", REDIRECT_URI)
            .finish();

        let response = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .context("Failed to send token exchange request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Token exchange failed (HTTP {status}): {body}");
        }

        let token_data: TokenResponse = response
            .json()
            .await
            .context("Failed to parse token response")?;

        let expires_at = compute_expires_at(token_data.expires_in);
        let account_id = decode_account_id(&token_data.access_token);

        Ok(OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: token_data.refresh_token,
            access: token_data.access_token,
            expires: expires_at,
            account_id,
        })
    }

    /// Refreshes an expired access token.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn refresh_token(refresh_token: &str) -> Result<OAuthCredentials> {
        let client = reqwest::Client::new();
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("refresh_token", refresh_token)
            .finish();

        let response = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .context("Failed to send token refresh request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed (HTTP {status}): {body}");
        }

        let token_data: TokenResponse = response
            .json()
            .await
            .context("Failed to parse token response")?;

        let expires_at = compute_expires_at(token_data.expires_in);
        let account_id = decode_account_id(&token_data.access_token);

        Ok(OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: token_data.refresh_token,
            access: token_data.access_token,
            expires: expires_at,
            account_id,
        })
    }

    #[derive(Debug, Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: u64,
    }

    fn compute_expires_at(expires_in_secs: u64) -> u64 {
        let now = super::now_millis_u64();
        now + (expires_in_secs * 1000).saturating_sub(5 * 60 * 1000)
    }

    fn decode_account_id(token: &str) -> Option<String> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        let payload = parts[1];
        let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
        let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
        let claim = json.get(JWT_CLAIM_PATH)?;
        claim
            .get("chatgpt_account_id")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string)
    }

    /// Loads the `OpenAI` Codex OAuth credentials from cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn load_credentials() -> Result<Option<OAuthCredentials>> {
        let cache = OAuthCache::load()?;
        Ok(cache.get(PROVIDER_KEY).cloned())
    }

    /// Saves `OpenAI` Codex OAuth credentials to cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_credentials(creds: &OAuthCredentials) -> Result<()> {
        let mut cache = OAuthCache::load()?;
        cache.set(PROVIDER_KEY, creds.clone());
        cache.save()?;
        Ok(())
    }

    /// Removes the `OpenAI` Codex OAuth credentials from cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn clear_credentials() -> Result<bool> {
        let mut cache = OAuthCache::load()?;
        let had_creds = cache.remove(PROVIDER_KEY).is_some();
        cache.save()?;
        Ok(had_creds)
    }
}

/// Google Gemini CLI (Cloud Code Assist OAuth) helpers.
pub mod gemini_cli {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use sha2::{Digest, Sha256};

    use super::{Context, Deserialize, OAuthCache, OAuthCredentials, Result};

    /// Provider key for Gemini CLI in the OAuth cache.
    pub const PROVIDER_KEY: &str = "gemini-cli";

    /// Google OAuth client ID (public, from Gemini CLI)
    const CLIENT_ID: &str =
        "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";

    /// Google OAuth client secret (public, from Gemini CLI)
    const CLIENT_SECRET: &str = "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";

    /// Google OAuth URLs
    const AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
    const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
    const REDIRECT_URI: &str = "http://localhost:8085/oauth2callback";

    /// OAuth scopes for Cloud Code Assist
    const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile";

    /// Cloud Code Assist API endpoint
    const CLOUD_CODE_API: &str = "https://cloudcode-pa.googleapis.com";

    /// PKCE code verifier and challenge
    pub struct Pkce {
        pub verifier: String,
        pub challenge: String,
    }

    /// Gemini CLI credentials with project ID.
    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct GeminiCliCredentials {
        pub access: String,
        pub refresh: String,
        pub expires: u64,
        pub project_id: String,
    }

    /// Generate PKCE code verifier and challenge
    pub fn generate_pkce() -> Pkce {
        let uuid1 = uuid::Uuid::new_v4();
        let uuid2 = uuid::Uuid::new_v4();
        let mut verifier_bytes = [0u8; 32];
        verifier_bytes[..16].copy_from_slice(uuid1.as_bytes());
        verifier_bytes[16..].copy_from_slice(uuid2.as_bytes());
        let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

        Pkce {
            verifier,
            challenge,
        }
    }

    /// Build the authorization URL for Google OAuth
    pub fn build_auth_url(pkce: &Pkce, state: &str) -> String {
        let params = [
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ];

        let query: String = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(params)
            .finish();

        format!("{AUTHORIZE_URL}?{query}")
    }

    /// Parses a pasted authorization input into code + optional state.
    pub fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
        let value = input.trim();
        if value.is_empty() {
            return (None, None);
        }

        if let Ok(url) = url::Url::parse(value) {
            let code = url.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v);
            let state = url
                .query_pairs()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v);
            return (code.map(|v| v.to_string()), state.map(|v| v.to_string()));
        }

        if let Some((code, state)) = value.split_once('#') {
            return (Some(code.to_string()), Some(state.to_string()));
        }

        if value.contains("code=") {
            let params = url::form_urlencoded::parse(value.as_bytes()).collect::<Vec<_>>();
            let code = params.iter().find(|(k, _)| k == "code").map(|(_, v)| v);
            let state = params.iter().find(|(k, _)| k == "state").map(|(_, v)| v);
            return (
                code.map(std::string::ToString::to_string),
                state.map(std::string::ToString::to_string),
            );
        }

        (Some(value.to_string()), None)
    }

    /// Exchanges authorization code for tokens.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn exchange_code(auth_code: &str, pkce: &Pkce) -> Result<OAuthCredentials> {
        let client = reqwest::Client::new();
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "authorization_code")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("client_secret", CLIENT_SECRET)
            .append_pair("code", auth_code)
            .append_pair("code_verifier", &pkce.verifier)
            .append_pair("redirect_uri", REDIRECT_URI)
            .finish();

        let response = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .context("Failed to send token exchange request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Token exchange failed (HTTP {status}): {body}");
        }

        let token_data: TokenResponse = response
            .json()
            .await
            .context("Failed to parse token response")?;

        let expires_at = compute_expires_at(token_data.expires_in);

        Ok(OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: token_data.refresh_token.unwrap_or_default(),
            access: token_data.access_token,
            expires: expires_at,
            account_id: None, // Will be set after project discovery
        })
    }

    /// Refreshes an expired access token.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn refresh_token(refresh_token: &str, project_id: &str) -> Result<OAuthCredentials> {
        let client = reqwest::Client::new();
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("client_secret", CLIENT_SECRET)
            .append_pair("refresh_token", refresh_token)
            .finish();

        let response = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .context("Failed to send token refresh request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed (HTTP {status}): {body}");
        }

        let token_data: TokenResponse = response
            .json()
            .await
            .context("Failed to parse token response")?;

        let expires_at = compute_expires_at(token_data.expires_in);

        Ok(OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: token_data
                .refresh_token
                .unwrap_or_else(|| refresh_token.to_string()),
            access: token_data.access_token,
            expires: expires_at,
            account_id: Some(project_id.to_string()),
        })
    }

    /// Discovers or provisions a Cloud Code Assist project.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub async fn discover_project(access_token: &str) -> Result<String> {
        let client = reqwest::Client::new();

        let metadata = serde_json::json!({
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI",
        });

        // Try to load existing project first
        let load_url = format!("{CLOUD_CODE_API}/v1internal:loadCodeAssist");
        let response = client
            .post(&load_url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Content-Type", "application/json")
            .body(serde_json::json!({ "metadata": metadata.clone() }).to_string())
            .send()
            .await
            .context("Failed to load Cloud Code Assist project")?;

        if response.status().is_success() {
            let data: serde_json::Value = response.json().await.unwrap_or_default();
            if let Some(project) = data.get("cloudaicompanionProject").and_then(|v| v.as_str()) {
                return Ok(project.to_string());
            }

            let tier_id = data
                .get("allowedTiers")
                .and_then(|v| v.as_array())
                .and_then(|tiers| {
                    tiers
                        .iter()
                        .find(|tier| {
                            tier.get("isDefault").and_then(serde_json::Value::as_bool) == Some(true)
                        })
                        .or_else(|| tiers.first())
                })
                .and_then(|tier| tier.get("id").and_then(|v| v.as_str()))
                .unwrap_or("FREE")
                .to_string();

            // Try to onboard user (provision new project) with retries.
            let onboard_url = format!("{CLOUD_CODE_API}/v1internal:onboardUser");
            for attempt in 0..10 {
                let response = client
                    .post(&onboard_url)
                    .header("Authorization", format!("Bearer {access_token}"))
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "tierId": tier_id.clone(),
                            "metadata": metadata.clone(),
                        })
                        .to_string(),
                    )
                    .send()
                    .await
                    .context("Failed to onboard user for Cloud Code Assist")?;

                if response.status().is_success() {
                    let data: serde_json::Value = response
                        .json()
                        .await
                        .context("Failed to parse onboard response")?;
                    let done = data.get("done").and_then(serde_json::Value::as_bool) == Some(true);
                    let project_id = data
                        .get("response")
                        .and_then(|v| v.get("cloudaicompanionProject"))
                        .and_then(|v| v.get("id"))
                        .and_then(|v| v.as_str());
                    if done && let Some(project_id) = project_id {
                        return Ok(project_id.to_string());
                    }
                }

                if attempt < 9 {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
            }
        }

        anyhow::bail!(
            "Could not discover or provision a Google Cloud project. Ensure you have access to Google Cloud Code Assist."
        )
    }

    #[derive(Debug, Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: u64,
    }

    fn compute_expires_at(expires_in_secs: u64) -> u64 {
        let now = super::now_millis_u64();
        now + (expires_in_secs * 1000).saturating_sub(5 * 60 * 1000)
    }

    /// Loads the Gemini CLI OAuth credentials from cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn load_credentials() -> Result<Option<OAuthCredentials>> {
        let cache = OAuthCache::load()?;
        Ok(cache.get(PROVIDER_KEY).cloned())
    }

    /// Saves Gemini CLI OAuth credentials to cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_credentials(creds: &OAuthCredentials) -> Result<()> {
        let mut cache = OAuthCache::load()?;
        cache.set(PROVIDER_KEY, creds.clone());
        cache.save()?;
        Ok(())
    }

    /// Removes the Gemini CLI OAuth credentials from cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn clear_credentials() -> Result<bool> {
        let mut cache = OAuthCache::load()?;
        let had_creds = cache.remove(PROVIDER_KEY).is_some();
        cache.save()?;
        Ok(had_creds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: `OAuthCredentials` expiry check.
    #[test]
    fn test_credentials_expiry() {
        let now = now_millis_u64();

        let expired = OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: "refresh".to_string(),
            access: "access".to_string(),
            expires: now - 1000, // 1 second ago
            account_id: None,
        };
        assert!(expired.is_expired());

        let valid = OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: "refresh".to_string(),
            access: "access".to_string(),
            expires: now + 60000, // 1 minute from now
            account_id: None,
        };
        assert!(!valid.is_expired());
    }

    /// Test: `OAuthCache` serialization roundtrip (in-memory, no fs).
    #[test]
    fn test_oauth_cache_serialization() {
        let mut cache = OAuthCache::default();
        cache.set(
            "claude-cli",
            OAuthCredentials {
                cred_type: "oauth".to_string(),
                refresh: "refresh-token".to_string(),
                access: "access-token".to_string(),
                expires: 1_234_567_890_000,
                account_id: None,
            },
        );

        let json = serde_json::to_string(&cache).unwrap();
        let loaded: OAuthCache = serde_json::from_str(&json).unwrap();

        let creds = loaded.get("claude-cli").unwrap();
        assert_eq!(creds.cred_type, "oauth");
        assert_eq!(creds.access, "access-token");
        assert_eq!(creds.refresh, "refresh-token");
    }

    /// Test: `OAuthCache` remove.
    #[test]
    fn test_oauth_cache_remove() {
        let mut cache = OAuthCache::default();
        cache.set(
            "claude-cli",
            OAuthCredentials {
                cred_type: "oauth".to_string(),
                refresh: "r".to_string(),
                access: "a".to_string(),
                expires: 0,
                account_id: None,
            },
        );
        assert!(cache.get("claude-cli").is_some());

        let removed = cache.remove("claude-cli");
        assert!(removed.is_some());
        assert!(cache.get("claude-cli").is_none());
    }

    /// Test: Token masking.
    #[test]
    fn test_mask_token() {
        assert_eq!(
            claude_cli::mask_token("sk-ant-oat-long-token-here"),
            "sk-ant-oat-l..."
        );
        assert_eq!(claude_cli::mask_token("short"), "***");
    }

    /// Test: PKCE generation produces valid output.
    #[test]
    fn test_pkce_generation() {
        let pkce = claude_cli::generate_pkce();
        assert!(!pkce.verifier.is_empty());
        assert!(!pkce.challenge.is_empty());
        // Verifier should be base64url encoded 32 bytes = 43 chars
        assert!(pkce.verifier.len() >= 40);
    }

    /// Test: Auth URL contains required parameters.
    #[test]
    fn test_auth_url_format() {
        let pkce = claude_cli::generate_pkce();
        let redirect_uri = claude_cli::build_redirect_uri(55555);
        let url = claude_cli::build_auth_url(&pkce, "state", &redirect_uri);

        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(url.contains("client_id="));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
    }
}
