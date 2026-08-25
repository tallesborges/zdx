//! OAuth token storage and retrieval.
//!
//! Stores OAuth tokens in `<base>/oauth.json` with restricted permissions (0600).
//! Tokens are never logged or displayed in full.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// OAuth token cache filename.
const OAUTH_CACHE_FILE: &str = "oauth.json";

/// Lock file guarding read-modify-write access to the OAuth cache.
const OAUTH_LOCK_FILE: &str = "oauth.lock";

/// How long to wait for the cache lock before giving up.
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// How often to retry a contended cache lock.
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Returns the ZDX home directory (`$ZDX_HOME` or `~/.zdx`).
fn zdx_home() -> PathBuf {
    if let Ok(home) = std::env::var("ZDX_HOME") {
        return PathBuf::from(home);
    }

    // HOME on Unix, USERPROFILE on Windows
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .expect("Could not determine home directory");
    home.join(".zdx")
}

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

/// Separator between a provider key and a named account in the cache key.
pub const ACCOUNT_SEPARATOR: char = '@';

/// Builds the OAuth cache key for a provider + optional named account.
///
/// The default (unnamed) account uses the bare provider key, so existing
/// single-account caches keep working untouched.
pub fn account_cache_key(provider_key: &str, account: Option<&str>) -> String {
    match account {
        Some(name) => format!("{provider_key}{ACCOUNT_SEPARATOR}{name}"),
        None => provider_key.to_string(),
    }
}

/// Splits a cache key into its provider key and optional account name.
pub fn split_cache_key(key: &str) -> (&str, Option<&str>) {
    match key.split_once(ACCOUNT_SEPARATOR) {
        Some((provider, account)) if !account.is_empty() => (provider, Some(account)),
        _ => (key, None),
    }
}

/// Validates and normalizes an account name.
///
/// `None` and empty names both mean the default account.
///
/// # Errors
/// Returns an error if the name contains characters reserved by the model
/// grammar (`@`, `:`, `/`) or surrounding whitespace only.
pub fn normalize_account(account: Option<&str>) -> Result<Option<String>> {
    let Some(name) = account.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(None);
    };

    if name.contains([ACCOUNT_SEPARATOR, ':', '/']) {
        anyhow::bail!("Account name '{name}' cannot contain '@', ':' or '/'");
    }

    Ok(Some(name.to_string()))
}

/// OAuth token cache structure.
/// Maps provider names to their credentials.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OAuthCache {
    /// Provider name -> credentials mapping.
    #[serde(flatten)]
    pub providers: HashMap<String, OAuthCredentials>,
}

/// Cross-process guard around read-modify-write access to `oauth.json`.
///
/// Providers rotate the refresh token on every refresh, so two zdx processes
/// (bot daemon, TUI, subagent runs) refreshing the same account concurrently
/// would leave the loser persisting a token the provider already invalidated.
/// The lock also keeps a login/logout from clobbering another account's entry,
/// since the whole file is rewritten on every change.
pub struct CacheLock {
    #[cfg(unix)]
    file: fs::File,
}

fn lock_path() -> PathBuf {
    zdx_home().join(OAUTH_LOCK_FILE)
}

/// Writes `contents` to `path`, owner-readable only, flushed to disk.
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("Failed to open {} for writing", path.display()))?;
        file.write_all(contents)
            .with_context(|| format!("Failed to write to {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("Failed to flush {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        fs::write(path, contents)
            .with_context(|| format!("Failed to write to {}", path.display()))?;
    }

    Ok(())
}

#[cfg(unix)]
impl CacheLock {
    /// Acquires the lock, blocking the current thread until it is free.
    ///
    /// # Errors
    /// Returns an error if the lock file cannot be opened or stays contended
    /// past [`LOCK_TIMEOUT`].
    pub fn acquire_blocking() -> Result<Self> {
        let file = open_lock_file()?;
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            if try_lock(&file)? {
                return Ok(Self { file });
            }
            check_deadline(deadline)?;
            std::thread::sleep(LOCK_POLL_INTERVAL);
        }
    }

    /// Acquires the lock, yielding to the async runtime while waiting.
    ///
    /// # Errors
    /// Returns an error if the lock file cannot be opened or stays contended
    /// past [`LOCK_TIMEOUT`].
    pub async fn acquire() -> Result<Self> {
        let file = open_lock_file()?;
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            if try_lock(&file)? {
                return Ok(Self { file });
            }
            check_deadline(deadline)?;
            tokio::time::sleep(LOCK_POLL_INTERVAL).await;
        }
    }
}

#[cfg(unix)]
impl Drop for CacheLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        // SAFETY: the fd is owned by `self.file`, which outlives this call.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(unix)]
fn open_lock_file() -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    let path = lock_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("Failed to open OAuth lock file {}", path.display()))
}

#[cfg(unix)]
fn try_lock(file: &fs::File) -> Result<bool> {
    use std::os::unix::io::AsRawFd;

    // SAFETY: the fd is owned by `file`, which outlives this call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Ok(false);
    }
    Err(anyhow::Error::new(err).context("Failed to lock the OAuth cache"))
}

#[cfg(unix)]
fn check_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        anyhow::bail!(
            "Timed out after {}s waiting for the OAuth cache lock at {}",
            LOCK_TIMEOUT.as_secs(),
            lock_path().display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
impl CacheLock {
    /// Acquires the lock. File locking is only implemented on Unix.
    ///
    /// # Errors
    /// Never returns an error on this platform.
    pub fn acquire_blocking() -> Result<Self> {
        Ok(Self {})
    }

    /// Acquires the lock. File locking is only implemented on Unix.
    ///
    /// # Errors
    /// Never returns an error on this platform.
    pub async fn acquire() -> Result<Self> {
        Ok(Self {})
    }
}

/// Refreshes the stored credentials for `provider_key`/`account` while holding
/// the cache lock.
///
/// The cache is re-read after the lock is taken so a refresh another process
/// already completed is reused, instead of replaying a refresh token that the
/// provider has since rotated away.
///
/// # Errors
/// Returns an error if the lock cannot be taken, no credentials are stored for
/// the account, the refresh call fails, or the cache cannot be written.
pub async fn refresh_locked<F, Fut>(
    provider_key: &str,
    account: Option<&str>,
    refresh: F,
) -> Result<OAuthCredentials>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<OAuthCredentials>>,
{
    let _lock = CacheLock::acquire().await?;

    let key = account_cache_key(provider_key, account);
    let mut cache = OAuthCache::load()?;
    let current = cache
        .get(&key)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("No OAuth credentials stored for '{key}'"))?;

    if !current.is_expired() {
        return Ok(current);
    }

    let mut refreshed = refresh(current.refresh.clone()).await?;
    if refreshed.account_id.is_none() {
        refreshed.account_id = current.account_id;
    }
    cache.set(&key, refreshed.clone());
    cache.save()?;

    Ok(refreshed)
}

impl OAuthCache {
    /// Returns the path to the OAuth cache file.
    pub fn cache_path() -> PathBuf {
        zdx_home().join(OAUTH_CACHE_FILE)
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
    /// The JSON is staged in a same-directory temp file and renamed into place,
    /// so a crash or a concurrent reader never sees a half-written cache.
    /// Callers that read-modify-write must hold a [`CacheLock`]; prefer
    /// [`OAuthCache::update`].
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

        let tmp_path = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
        write_private(&tmp_path, contents.as_bytes())?;

        if let Err(err) = fs::rename(&tmp_path, &path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(
                anyhow::Error::new(err).context(format!("Failed to replace {}", path.display()))
            );
        }

        Ok(())
    }

    /// Applies `f` to the on-disk cache under the cache lock, then saves it.
    ///
    /// This is the only safe way to change a single entry: the whole file is
    /// rewritten on save, so an unlocked read-modify-write can drop another
    /// account's credentials written in between.
    ///
    /// # Errors
    /// Returns an error if the lock cannot be taken or the cache cannot be
    /// read or written.
    pub fn update<T>(f: impl FnOnce(&mut Self) -> T) -> Result<T> {
        let _lock = CacheLock::acquire_blocking()?;
        let mut cache = Self::load()?;
        let out = f(&mut cache);
        cache.save()?;
        Ok(out)
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

    /// Lists the accounts stored for a provider key, default account first.
    ///
    /// `None` represents the default (unnamed) account.
    pub fn accounts(&self, provider_key: &str) -> Vec<Option<String>> {
        let mut accounts: Vec<Option<String>> = self
            .providers
            .keys()
            .filter_map(|key| {
                let (provider, account) = split_cache_key(key);
                (provider == provider_key).then(|| account.map(ToString::to_string))
            })
            .collect();
        accounts.sort();
        accounts
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

    /// Loads the Claude CLI OAuth credentials for an account from cache.
    ///
    /// `None` selects the default account.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn load_credentials(account: Option<&str>) -> Result<Option<OAuthCredentials>> {
        let cache = OAuthCache::load()?;
        Ok(cache
            .get(&super::account_cache_key(PROVIDER_KEY, account))
            .cloned())
    }

    /// Saves Claude CLI OAuth credentials for an account to cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_credentials(account: Option<&str>, creds: &OAuthCredentials) -> Result<()> {
        OAuthCache::update(|cache| {
            cache.set(
                &super::account_cache_key(PROVIDER_KEY, account),
                creds.clone(),
            );
        })
    }

    /// Removes the Claude CLI OAuth credentials for an account from cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn clear_credentials(account: Option<&str>) -> Result<bool> {
        OAuthCache::update(|cache| {
            cache
                .remove(&super::account_cache_key(PROVIDER_KEY, account))
                .is_some()
        })
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

    /// Loads the `OpenAI` Codex OAuth credentials for an account from cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn load_credentials(account: Option<&str>) -> Result<Option<OAuthCredentials>> {
        let cache = OAuthCache::load()?;
        Ok(cache
            .get(&super::account_cache_key(PROVIDER_KEY, account))
            .cloned())
    }

    /// Saves `OpenAI` Codex OAuth credentials for an account to cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_credentials(account: Option<&str>, creds: &OAuthCredentials) -> Result<()> {
        OAuthCache::update(|cache| {
            cache.set(
                &super::account_cache_key(PROVIDER_KEY, account),
                creds.clone(),
            );
        })
    }

    /// Removes the `OpenAI` Codex OAuth credentials for an account from cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn clear_credentials(account: Option<&str>) -> Result<bool> {
        OAuthCache::update(|cache| {
            cache
                .remove(&super::account_cache_key(PROVIDER_KEY, account))
                .is_some()
        })
    }
}

/// Google Antigravity (Cloud Code Assist sandbox OAuth) helpers.
pub mod google_antigravity {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use sha2::{Digest, Sha256};

    use super::{Context, Deserialize, OAuthCache, OAuthCredentials, Result};

    pub const PROVIDER_KEY: &str = "google-antigravity";

    const CLIENT_ID: &str =
        "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
    const CLIENT_SECRET: &str = "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";
    const AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
    const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
    const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
    const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";
    const DEFAULT_PROJECT_ID: &str = "rising-fact-p41fc";

    pub struct Pkce {
        pub verifier: String,
        pub challenge: String,
    }

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct AntigravityCredentials {
        pub access: String,
        pub refresh: String,
        pub expires: u64,
        pub project_id: String,
    }

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
            .context("Failed to send Antigravity token exchange request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Antigravity token exchange failed (HTTP {status}): {body}");
        }

        let token_data: TokenResponse = response
            .json()
            .await
            .context("Failed to parse Antigravity token response")?;
        let expires_at = compute_expires_at(token_data.expires_in);

        Ok(OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: token_data.refresh_token.unwrap_or_default(),
            access: token_data.access_token,
            expires: expires_at,
            account_id: None,
        })
    }

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
            .context("Failed to send Antigravity token refresh request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Antigravity token refresh failed (HTTP {status}): {body}");
        }

        let token_data: TokenResponse = response
            .json()
            .await
            .context("Failed to parse Antigravity token response")?;
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
        let endpoints = [
            "https://cloudcode-pa.googleapis.com",
            "https://daily-cloudcode-pa.googleapis.com",
        ];

        for endpoint in endpoints {
            let load_url = format!("{endpoint}/v1internal:loadCodeAssist");
            let response = client
                .post(&load_url)
                .header("Authorization", format!("Bearer {access_token}"))
                .header("Content-Type", "application/json")
                .header("User-Agent", "google-api-nodejs-client/9.15.1")
                .header(
                    "X-Goog-Api-Client",
                    "google-cloud-sdk vscode_cloudshelleditor/0.1",
                )
                .header("Client-Metadata", metadata.to_string())
                .json(&serde_json::json!({ "metadata": metadata.clone() }))
                .send()
                .await;

            let Ok(response) = response else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            let data: serde_json::Value = response.json().await.unwrap_or_default();
            if let Some(project) = data
                .get("cloudaicompanionProject")
                .and_then(serde_json::Value::as_str)
            {
                return Ok(project.to_string());
            }
            if let Some(project) = data
                .get("cloudaicompanionProject")
                .and_then(|value| value.get("id"))
                .and_then(serde_json::Value::as_str)
            {
                return Ok(project.to_string());
            }
        }

        Ok(DEFAULT_PROJECT_ID.to_string())
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

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn load_credentials(account: Option<&str>) -> Result<Option<OAuthCredentials>> {
        let cache = OAuthCache::load()?;
        Ok(cache
            .get(&super::account_cache_key(PROVIDER_KEY, account))
            .cloned())
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_credentials(account: Option<&str>, creds: &OAuthCredentials) -> Result<()> {
        OAuthCache::update(|cache| {
            cache.set(
                &super::account_cache_key(PROVIDER_KEY, account),
                creds.clone(),
            );
        })
    }

    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn clear_credentials(account: Option<&str>) -> Result<bool> {
        OAuthCache::update(|cache| {
            cache
                .remove(&super::account_cache_key(PROVIDER_KEY, account))
                .is_some()
        })
    }
}

/// Grok Build (xAI Grok subscription OAuth) helpers.
///
/// Uses the public Grok CLI desktop OAuth client to authenticate against a
/// `SuperGrok` / X Premium+ subscription. The resulting access token is a bearer
/// against the standard xAI Responses API (`https://api.x.ai/v1`).
pub mod grok_build {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use sha2::{Digest, Sha256};

    use super::{Context, Deserialize, OAuthCache, OAuthCredentials, Result};

    /// Provider key for Grok Build in the OAuth cache.
    pub const PROVIDER_KEY: &str = "grok-build";

    /// Public Grok CLI desktop OAuth client ID (not a secret).
    const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

    /// xAI OAuth URLs (from `https://auth.x.ai/.well-known/openid-configuration`).
    const AUTHORIZE_URL: &str = "https://auth.x.ai/oauth2/authorize";
    const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
    /// Local OAuth callback used by the Grok CLI flow.
    pub const LOCAL_CALLBACK_PORT: u16 = 56121;
    pub const LOCAL_CALLBACK_PATH: &str = "/callback";
    const REDIRECT_URI: &str = "http://127.0.0.1:56121/callback";
    const SCOPES: &str = "openid profile email offline_access grok-cli:access api:access";

    /// PKCE code verifier and challenge.
    pub struct Pkce {
        pub verifier: String,
        pub challenge: String,
    }

    /// Generate PKCE code verifier and challenge.
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

    /// Build the authorization URL for the Grok Build OAuth flow.
    ///
    /// `plan=generic` and `referrer` are attribution tags the desktop flow
    /// expects; `referrer` is free-form and not tied to the client ID (other
    /// clients send their own name, e.g. `cli-proxy-api`, `hermes-agent`).
    pub fn build_auth_url(pkce: &Pkce, state: &str) -> String {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let params = [
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("nonce", &nonce),
            ("plan", "generic"),
            ("referrer", "zdx"),
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

    /// Exchanges an authorization code for tokens.
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
            .header("Accept", "application/json")
            .body(body)
            .send()
            .await
            .context("Failed to send Grok Build token exchange request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Grok Build token exchange failed (HTTP {status}): {body}");
        }

        let token_data: TokenResponse = response
            .json()
            .await
            .context("Failed to parse Grok Build token response")?;
        let expires_at = compute_expires_at(token_data.expires_in);

        Ok(OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: token_data.refresh_token.unwrap_or_default(),
            access: token_data.access_token,
            expires: expires_at,
            account_id: None,
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
            .header("Accept", "application/json")
            .body(body)
            .send()
            .await
            .context("Failed to send Grok Build token refresh request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Grok Build token refresh failed (HTTP {status}): {body}");
        }

        let token_data: TokenResponse = response
            .json()
            .await
            .context("Failed to parse Grok Build token response")?;
        let expires_at = compute_expires_at(token_data.expires_in);

        Ok(OAuthCredentials {
            cred_type: "oauth".to_string(),
            refresh: token_data
                .refresh_token
                .unwrap_or_else(|| refresh_token.to_string()),
            access: token_data.access_token,
            expires: expires_at,
            account_id: None,
        })
    }

    #[derive(Debug, Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: Option<u64>,
    }

    fn compute_expires_at(expires_in_secs: Option<u64>) -> u64 {
        let now = super::now_millis_u64();
        let expires_in = expires_in_secs.unwrap_or(3600);
        now + (expires_in * 1000).saturating_sub(5 * 60 * 1000)
    }

    /// Loads the Grok Build OAuth credentials for an account from cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn load_credentials(account: Option<&str>) -> Result<Option<OAuthCredentials>> {
        let cache = OAuthCache::load()?;
        Ok(cache
            .get(&super::account_cache_key(PROVIDER_KEY, account))
            .cloned())
    }

    /// Saves Grok Build OAuth credentials for an account to cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn save_credentials(account: Option<&str>, creds: &OAuthCredentials) -> Result<()> {
        OAuthCache::update(|cache| {
            cache.set(
                &super::account_cache_key(PROVIDER_KEY, account),
                creds.clone(),
            );
        })
    }

    /// Removes the Grok Build OAuth credentials for an account from cache.
    ///
    /// # Errors
    /// Returns an error if the operation fails.
    pub fn clear_credentials(account: Option<&str>) -> Result<bool> {
        OAuthCache::update(|cache| {
            cache
                .remove(&super::account_cache_key(PROVIDER_KEY, account))
                .is_some()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_cache_keys_round_trip() {
        assert_eq!(account_cache_key("claude-cli", None), "claude-cli");
        assert_eq!(
            account_cache_key("claude-cli", Some("work")),
            "claude-cli@work"
        );
        assert_eq!(split_cache_key("claude-cli"), ("claude-cli", None));
        assert_eq!(
            split_cache_key("claude-cli@work"),
            ("claude-cli", Some("work"))
        );
    }

    #[test]
    fn cache_lists_default_account_first() {
        let mut cache = OAuthCache::default();
        for key in ["claude-cli@work", "claude-cli", "openai-codex"] {
            cache.set(
                key,
                OAuthCredentials {
                    cred_type: "oauth".to_string(),
                    refresh: "r".to_string(),
                    access: "a".to_string(),
                    expires: 0,
                    account_id: None,
                },
            );
        }

        assert_eq!(
            cache.accounts("claude-cli"),
            vec![None, Some("work".to_string())]
        );
    }

    #[test]
    fn normalize_account_rejects_reserved_characters() {
        assert!(normalize_account(None).unwrap().is_none());
        assert!(normalize_account(Some("  ")).unwrap().is_none());
        assert_eq!(
            normalize_account(Some(" work ")).unwrap(),
            Some("work".to_string())
        );
        assert!(normalize_account(Some("we:ird")).is_err());
        assert!(normalize_account(Some("we@ird")).is_err());
    }

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
