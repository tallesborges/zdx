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
) -> (Vec<StateCommand>, LoginOverlayAction) {
    auth.login_rx = None;
    match result {
        Ok(()) => {
            auth.refresh();
            (
                vec![StateCommand::Transcript(
                    TranscriptCommand::AppendSystemMessage(
                        "Logged in with Anthropic OAuth.".to_string(),
                    ),
                )],
                LoginOverlayAction::Close,
            )
        }
        Err(msg) => (vec![], LoginOverlayAction::Reopen { error: msg }),
    }
}
