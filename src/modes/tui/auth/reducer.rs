//! Auth feature reducer.
//!
//! Handles login flow state transitions and result processing.

use crate::modes::tui::overlays::{LoginState, Overlay};
use crate::modes::tui::state::TuiState;
use crate::modes::tui::transcript::HistoryCell;

/// Handles the login result from OAuth token exchange.
///
/// Updates auth state and transcript based on success or failure.
pub fn handle_login_result(
    tui: &mut TuiState,
    overlay: &mut Option<Overlay>,
    result: Result<(), String>,
) {
    use crate::providers::oauth::anthropic;

    tui.auth.login_rx = None;
    match result {
        Ok(()) => {
            *overlay = None;
            tui.refresh_auth_type();
            tui.transcript
                .cells
                .push(HistoryCell::system("Logged in with Anthropic OAuth."));
        }
        Err(msg) => {
            let pkce = anthropic::generate_pkce();
            let url = anthropic::build_auth_url(&pkce);
            *overlay = Some(Overlay::Login(LoginState::AwaitingCode {
                url,
                pkce_verifier: pkce.verifier,
                input: String::new(),
                error: Some(msg),
            }));
        }
    }
}
