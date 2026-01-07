//! Authentication state.
//!
//! Manages authentication type detection and login flow state.

use crate::modes::tui::shared::internal::AuthMutation;

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
///
/// With the inbox pattern, login results arrive directly via the runtime's
/// event inbox, so we no longer need per-operation receivers.
pub struct AuthState {
    /// Current auth type indicator (cached, refreshed on login/logout).
    pub auth_type: AuthStatus,

    /// Whether a login exchange is in progress.
    pub login_in_progress: bool,

    /// Whether a local OAuth callback is being awaited.
    pub callback_in_progress: bool,
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
            login_in_progress: false,
            callback_in_progress: false,
        }
    }

    /// Refreshes the auth type by re-detecting it.
    pub fn refresh(&mut self) {
        self.auth_type = AuthStatus::detect();
    }

    /// Applies a cross-slice auth mutation.
    pub fn apply(&mut self, mutation: AuthMutation) {
        match mutation {
            AuthMutation::RefreshStatus => self.refresh(),
            AuthMutation::SetLoginInProgress(in_progress) => self.login_in_progress = in_progress,
            AuthMutation::SetCallbackInProgress(in_progress) => {
                self.callback_in_progress = in_progress
            }
        }
    }
}
