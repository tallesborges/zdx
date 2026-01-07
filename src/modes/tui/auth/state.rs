//! Authentication state.
//!
//! Manages authentication type detection and login flow state.

use tokio::sync::mpsc;

use crate::modes::tui::shared::internal::AuthCommand;

/// Authentication type indicator for status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStatus {
    /// Using OAuth token from <base>/oauth.json
    OAuth,
    /// Using API key from environment
    ApiKey,
    /// No authentication configured
    None,
}

impl AuthStatus {
    /// Detects the current authentication type.
    pub fn detect() -> Self {
        use crate::providers::oauth::{anthropic, openai_codex};

        // Check for OAuth credentials first
        if let Ok(Some(_creds)) = anthropic::load_credentials() {
            return AuthStatus::OAuth;
        }
        if let Ok(Some(_creds)) = openai_codex::load_credentials() {
            return AuthStatus::OAuth;
        }

        // Check for API key in environment
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            return AuthStatus::ApiKey;
        }

        AuthStatus::None
    }
}

/// Authentication state.
///
/// Encapsulates the current auth type and login flow state.
pub struct AuthState {
    /// Current auth type indicator (cached, refreshed on login/logout).
    pub auth_type: AuthStatus,

    /// Receiver for async login token exchange result.
    pub login_rx: Option<tokio::sync::mpsc::Receiver<Result<(), String>>>,

    /// Receiver for local OAuth callback (code) when available.
    pub login_callback_rx: Option<mpsc::Receiver<Option<String>>>,
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
            auth_type: AuthStatus::detect(),
            login_rx: None,
            login_callback_rx: None,
        }
    }

    /// Refreshes the auth type by re-detecting it.
    pub fn refresh(&mut self) {
        self.auth_type = AuthStatus::detect();
    }

    /// Applies a cross-slice auth command.
    pub fn apply(&mut self, command: AuthCommand) {
        match command {
            AuthCommand::RefreshStatus => self.refresh(),
            AuthCommand::ClearLoginRx => self.login_rx = None,
            AuthCommand::ClearLoginCallbackRx => self.login_callback_rx = None,
        }
    }
}
