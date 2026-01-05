use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::OverlayAction;
use crate::modes::tui::app::TuiState;
use crate::modes::tui::auth::render_login_overlay;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{AuthCommand, StateCommand};

#[derive(Debug, Clone)]
pub enum LoginState {
    AwaitingCode {
        url: String,
        pkce_verifier: String,
        input: String,
        error: Option<String>,
    },
    Exchanging,
}

impl LoginState {
    pub fn open() -> (Self, Vec<UiEffect>) {
        use crate::providers::oauth::anthropic;

        let pkce = anthropic::generate_pkce();
        let url = anthropic::build_auth_url(&pkce);
        let state = LoginState::AwaitingCode {
            url: url.clone(),
            pkce_verifier: pkce.verifier,
            input: String::new(),
            error: None,
        };
        (state, vec![UiEffect::OpenBrowser { url }])
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, _input_y: u16) {
        render_login_overlay(frame, self, area)
    }

    pub fn handle_key(
        &mut self,
        _tui: &TuiState,
        key: KeyEvent,
    ) -> (Option<OverlayAction>, Vec<StateCommand>) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match self {
            LoginState::AwaitingCode { input, .. } => match key.code {
                KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => (
                    Some(OverlayAction::close()),
                    vec![StateCommand::Auth(AuthCommand::ClearLoginRx)],
                ),
                KeyCode::Enter => {
                    let code = input.trim().to_string();
                    if code.is_empty() {
                        return (None, vec![]);
                    }

                    let verifier = match self {
                        LoginState::AwaitingCode { pkce_verifier, .. } => pkce_verifier.clone(),
                        _ => return (None, vec![]),
                    };

                    *self = LoginState::Exchanging;

                    (
                        Some(OverlayAction::Effects(vec![UiEffect::SpawnTokenExchange {
                            code,
                            verifier,
                        }])),
                        vec![],
                    )
                }
                KeyCode::Backspace => {
                    input.pop();
                    (None, vec![])
                }
                KeyCode::Char(c) if !ctrl => {
                    input.push(c);
                    (None, vec![])
                }
                _ => (None, vec![]),
            },
            LoginState::Exchanging => {
                if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
                    (
                        Some(OverlayAction::close()),
                        vec![StateCommand::Auth(AuthCommand::ClearLoginRx)],
                    )
                } else {
                    (None, vec![])
                }
            }
        }
    }
}
