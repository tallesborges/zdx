//! Auth feature reducer.
//!
//! Handles login flow state transitions and result processing.

use crate::auth::AuthState;
use crate::mutations::{StateMutation, TranscriptMutation};

/// Handles the login result from OAuth token exchange.
///
/// Updates auth state and transcript based on success or failure.
#[derive(Debug)]
pub enum LoginOverlayAction {
    Close,
    Reopen { error: String },
}

///
/// # Errors
/// Returns an error if the operation fails.
pub fn handle_login_result(
    auth: &mut AuthState,
    result: Result<(), String>,
    provider: zdx_engine::providers::ProviderKind,
) -> (Vec<StateMutation>, LoginOverlayAction) {
    match result {
        Ok(()) => {
            auth.refresh();
            let message = match provider {
                zdx_engine::providers::ProviderKind::ClaudeCli => {
                    "Logged in with Claude CLI OAuth."
                }
                zdx_engine::providers::ProviderKind::OpenAICodex => {
                    "Logged in with OpenAI Codex OAuth."
                }
                zdx_engine::providers::ProviderKind::GeminiCli => {
                    "Logged in with Gemini CLI OAuth."
                }
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
