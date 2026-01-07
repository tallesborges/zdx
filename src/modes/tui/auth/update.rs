//! Auth feature reducer.
//!
//! Handles login flow state transitions and result processing.

use crate::modes::tui::auth::AuthState;
use crate::modes::tui::shared::internal::{StateCommand, TranscriptCommand};

/// Handles the login result from OAuth token exchange.
///
/// Updates auth state and transcript based on success or failure.
#[derive(Debug)]
pub enum LoginOverlayAction {
    Close,
    Reopen { error: String },
}

pub fn handle_login_result(
    auth: &mut AuthState,
    result: Result<(), String>,
    provider: crate::providers::ProviderKind,
) -> (Vec<StateCommand>, LoginOverlayAction) {
    auth.login_rx = None;
    auth.login_callback_rx = None;
    match result {
        Ok(()) => {
            auth.refresh();
            let message = match provider {
                crate::providers::ProviderKind::Anthropic => "Logged in with Anthropic OAuth.",
                crate::providers::ProviderKind::OpenAICodex => "Logged in with OpenAI Codex OAuth.",
            };
            (
                vec![StateCommand::Transcript(
                    TranscriptCommand::AppendSystemMessage(message.to_string()),
                )],
                LoginOverlayAction::Close,
            )
        }
        Err(msg) => (vec![], LoginOverlayAction::Reopen { error: msg }),
    }
}
