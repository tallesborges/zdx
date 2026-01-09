//! Auth feature reducer.
//!
//! Handles login flow state transitions and result processing.

use crate::modes::tui::auth::AuthState;
use crate::modes::tui::shared::internal::{StateMutation, TranscriptMutation};

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
) -> (Vec<StateMutation>, LoginOverlayAction) {
    // Clear in-progress flags
    auth.callback_in_progress = false;
    match result {
        Ok(()) => {
            auth.refresh();
            let message = match provider {
                crate::providers::ProviderKind::ClaudeCli => "Logged in with Claude CLI OAuth.",
                crate::providers::ProviderKind::OpenAICodex => "Logged in with OpenAI Codex OAuth.",
                crate::providers::ProviderKind::GeminiCli => "Logged in with Gemini CLI OAuth.",
                _ => "Login complete.",
            };
            (
                vec![StateMutation::Transcript(
                    TranscriptMutation::AppendSystemMessage(message.to_string()),
                )],
                LoginOverlayAction::Close,
            )
        }
        Err(msg) => (vec![], LoginOverlayAction::Reopen { error: msg }),
    }
}
