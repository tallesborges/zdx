use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::OverlayAction;
use crate::modes::tui::app::TuiState;
use crate::modes::tui::auth::render_login_overlay;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{AuthMutation, StateMutation};
use crate::providers::{ProviderKind, provider_for_model};

#[derive(Debug, Clone)]
pub enum LoginState {
    AwaitingCode {
        provider: ProviderKind,
        url: String,
        pkce_verifier: String,
        oauth_state: Option<String>,
        input: String,
        error: Option<String>,
    },
    Exchanging {
        provider: ProviderKind,
    },
}

impl LoginState {
    pub fn open(tui: &TuiState) -> (Self, Vec<UiEffect>) {
        let provider = provider_for_model(&tui.config.model);
        Self::open_with_provider(provider, None, true)
    }

    pub fn reopen(provider: ProviderKind, error: String) -> Self {
        let (state, _) = Self::open_with_provider(provider, Some(error), false);
        state
    }

    fn open_with_provider(
        provider: ProviderKind,
        error: Option<String>,
        open_browser: bool,
    ) -> (Self, Vec<UiEffect>) {
        match provider {
            ProviderKind::Anthropic => {
                use crate::providers::oauth::anthropic;

                let pkce = anthropic::generate_pkce();
                let url = anthropic::build_auth_url(&pkce);
                let state = LoginState::AwaitingCode {
                    provider,
                    url: url.clone(),
                    pkce_verifier: pkce.verifier,
                    oauth_state: None,
                    input: String::new(),
                    error,
                };
                let effects = if open_browser {
                    vec![UiEffect::OpenBrowser { url }]
                } else {
                    vec![]
                };
                (state, effects)
            }
            ProviderKind::OpenAICodex => {
                use crate::providers::oauth::openai_codex;

                let pkce = openai_codex::generate_pkce();
                let oauth_state = uuid::Uuid::new_v4().to_string();
                let url = openai_codex::build_auth_url(&pkce, &oauth_state);
                let oauth_state_copy = oauth_state.clone();
                let state = LoginState::AwaitingCode {
                    provider,
                    url: url.clone(),
                    pkce_verifier: pkce.verifier,
                    oauth_state: Some(oauth_state),
                    input: String::new(),
                    error,
                };
                let mut effects = vec![UiEffect::StartLocalAuthCallback {
                    provider,
                    state: Some(oauth_state_copy),
                }];
                if open_browser {
                    effects.push(UiEffect::OpenBrowser { url });
                }
                (state, effects)
            }
        }
    }

    pub fn provider(&self) -> ProviderKind {
        match self {
            LoginState::AwaitingCode { provider, .. } => *provider,
            LoginState::Exchanging { provider } => *provider,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, _input_y: u16) {
        render_login_overlay(frame, self, area)
    }

    pub fn handle_key(
        &mut self,
        _tui: &TuiState,
        key: KeyEvent,
    ) -> (Option<OverlayAction>, Vec<StateMutation>) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match self {
            LoginState::AwaitingCode {
                provider,
                input,
                pkce_verifier,
                oauth_state,
                error,
                ..
            } => match key.code {
                KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => (
                    Some(OverlayAction::close()),
                    vec![
                        StateMutation::Auth(AuthMutation::ClearLoginRx),
                        StateMutation::Auth(AuthMutation::ClearLoginCallbackRx),
                    ],
                ),
                KeyCode::Enter => {
                    let provider = *provider;
                    let pkce_verifier = pkce_verifier.clone();
                    let oauth_state = oauth_state.clone();
                    let code = input.trim();
                    if code.is_empty() {
                        return (None, vec![]);
                    }

                    let code = match provider {
                        ProviderKind::Anthropic => code.to_string(),
                        ProviderKind::OpenAICodex => {
                            use crate::providers::oauth::openai_codex;

                            let (parsed_code, provided_state) =
                                openai_codex::parse_authorization_input(code);
                            if let Some(expected) = oauth_state
                                && let Some(provided) = provided_state
                                && provided != expected
                            {
                                *error = Some("State mismatch.".to_string());
                                return (None, vec![]);
                            }
                            match parsed_code {
                                Some(value) => value,
                                None => {
                                    *error =
                                        Some("Authorization code cannot be empty.".to_string());
                                    return (None, vec![]);
                                }
                            }
                        }
                    };

                    *self = LoginState::Exchanging { provider };

                    (
                        Some(OverlayAction::Effects(vec![UiEffect::SpawnTokenExchange {
                            provider,
                            code,
                            verifier: pkce_verifier,
                        }])),
                        vec![StateMutation::Auth(AuthMutation::ClearLoginCallbackRx)],
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
            LoginState::Exchanging { .. } => {
                if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
                    (
                        Some(OverlayAction::close()),
                        vec![
                            StateMutation::Auth(AuthMutation::ClearLoginRx),
                            StateMutation::Auth(AuthMutation::ClearLoginCallbackRx),
                        ],
                    )
                } else {
                    (None, vec![])
                }
            }
        }
    }
}
