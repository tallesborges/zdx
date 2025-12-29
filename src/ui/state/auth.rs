//! Authentication state.
//!
//! Manages authentication type detection and login flow state.

/// Authentication type indicator for status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    /// Using OAuth token from ~/.zdx/oauth.json
    OAuth,
    /// Using API key from environment
    ApiKey,
    /// No authentication configured
    None,
}

impl AuthType {
    /// Detects the current authentication type.
    pub fn detect() -> Self {
        use crate::providers::oauth::anthropic;

        // Check for OAuth credentials first
        if let Ok(Some(_creds)) = anthropic::load_credentials() {
            return AuthType::OAuth;
        }

        // Check for API key in environment
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            return AuthType::ApiKey;
        }

        AuthType::None
    }
}

/// Authentication state.
///
/// Encapsulates the current auth type and login flow state.
pub struct AuthState {
    /// Current auth type indicator (cached, refreshed on login/logout).
    pub auth_type: AuthType,

    /// Receiver for async login token exchange result.
    pub login_rx: Option<tokio::sync::mpsc::Receiver<Result<(), String>>>,
}

impl Default for AuthState {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthState {
    /// Creates a new AuthState by detecting the current auth type.
    pub fn new() -> Self {
        Self {
            auth_type: AuthType::detect(),
            login_rx: None,
        }
    }

    /// Refreshes the auth type by re-detecting it.
    pub fn refresh(&mut self) {
        self.auth_type = AuthType::detect();
    }
}
