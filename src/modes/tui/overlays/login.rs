use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::{OverlayEffect, OverlayUpdate};
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
        redirect_uri: Option<String>,
        input: String,
        error: Option<String>,
    },
    Exchanging {
        provider: ProviderKind,
    },
    ApiKeyInfo {
        provider: ProviderKind,
        env_var: String,
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
                let oauth_state = uuid::Uuid::new_v4().to_string();
                let callback_port = anthropic::random_local_port();
                let redirect_uri = anthropic::build_redirect_uri(callback_port);
                let url = anthropic::build_auth_url(&pkce, &oauth_state, &redirect_uri);
                let state = LoginState::AwaitingCode {
                    provider,
                    url: url.clone(),
                    pkce_verifier: pkce.verifier,
                    oauth_state: Some(oauth_state.clone()),
                    redirect_uri: Some(redirect_uri),
                    input: String::new(),
                    error,
                };
                let mut effects = vec![UiEffect::StartLocalAuthCallback {
                    provider,
                    state: Some(oauth_state),
                    port: Some(callback_port),
                }];
                if open_browser {
                    effects.push(UiEffect::OpenBrowser { url });
                }
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
                    redirect_uri: None,
                    input: String::new(),
                    error,
                };
                let mut effects = vec![UiEffect::StartLocalAuthCallback {
                    provider,
                    state: Some(oauth_state_copy),
                    port: None,
                }];
                if open_browser {
                    effects.push(UiEffect::OpenBrowser { url });
                }
                (state, effects)
            }
            ProviderKind::GeminiCli => {
                use crate::providers::oauth::gemini_cli;

                let pkce = gemini_cli::generate_pkce();
                let oauth_state = uuid::Uuid::new_v4().to_string();
                let url = gemini_cli::build_auth_url(&pkce, &oauth_state);
                let oauth_state_copy = oauth_state.clone();
                let state = LoginState::AwaitingCode {
                    provider,
                    url: url.clone(),
                    pkce_verifier: pkce.verifier,
                    oauth_state: Some(oauth_state),
                    redirect_uri: None,
                    input: String::new(),
                    error,
                };
                let mut effects = vec![UiEffect::StartLocalAuthCallback {
                    provider,
                    state: Some(oauth_state_copy),
                    port: None,
                }];
                if open_browser {
                    effects.push(UiEffect::OpenBrowser { url });
                }
                (state, effects)
            }
            _ => {
                let env_var = provider.api_key_env_var().unwrap_or("API_KEY").to_string();
                (LoginState::ApiKeyInfo { provider, env_var }, vec![])
            }
        }
    }

    pub fn provider(&self) -> ProviderKind {
        match self {
            LoginState::AwaitingCode { provider, .. } => *provider,
            LoginState::Exchanging { provider } => *provider,
            LoginState::ApiKeyInfo { provider, .. } => *provider,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, _input_y: u16) {
        render_login_overlay(frame, self, area)
    }

    pub fn handle_key(&mut self, _tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match self {
            LoginState::AwaitingCode {
                provider,
                input,
                pkce_verifier,
                oauth_state,
                redirect_uri,
                error,
                ..
            } => match key.code {
                KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                    OverlayUpdate::close().with_mutations(vec![
                        StateMutation::Auth(AuthMutation::CancelLoginRequest),
                        StateMutation::Auth(AuthMutation::SetCallbackInProgress(false)),
                    ])
                }
                KeyCode::Enter => {
                    let provider = *provider;
                    let pkce_verifier = pkce_verifier.clone();
                    let oauth_state = oauth_state.clone();
                    let redirect_uri = redirect_uri.clone();
                    let code = input.trim();
                    if code.is_empty() {
                        return OverlayUpdate::stay();
                    }

                    let code = match provider {
                        ProviderKind::Anthropic => {
                            use crate::providers::oauth::anthropic;

                            let (parsed_code, provided_state) =
                                anthropic::parse_authorization_input(code);
                            let expected_state =
                                oauth_state.clone().unwrap_or_else(|| pkce_verifier.clone());
                            if let Some(provided) = provided_state
                                && provided != expected_state
                            {
                                *error = Some("State mismatch.".to_string());
                                return OverlayUpdate::stay();
                            }
                            let parsed_code = match parsed_code {
                                Some(value) => value,
                                None => {
                                    *error =
                                        Some("Authorization code cannot be empty.".to_string());
                                    return OverlayUpdate::stay();
                                }
                            };
                            format!("{}#{}", parsed_code, expected_state)
                        }
                        ProviderKind::OpenAICodex => {
                            use crate::providers::oauth::openai_codex;

                            let (parsed_code, provided_state) =
                                openai_codex::parse_authorization_input(code);
                            if let Some(expected) = oauth_state
                                && let Some(provided) = provided_state
                                && provided != expected
                            {
                                *error = Some("State mismatch.".to_string());
                                return OverlayUpdate::stay();
                            }
                            match parsed_code {
                                Some(value) => value,
                                None => {
                                    *error =
                                        Some("Authorization code cannot be empty.".to_string());
                                    return OverlayUpdate::stay();
                                }
                            }
                        }
                        ProviderKind::GeminiCli => {
                            use crate::providers::oauth::gemini_cli;

                            let (parsed_code, provided_state) =
                                gemini_cli::parse_authorization_input(code);
                            if let Some(expected) = oauth_state
                                && let Some(provided) = provided_state
                                && provided != expected
                            {
                                *error = Some("State mismatch.".to_string());
                                return OverlayUpdate::stay();
                            }
                            match parsed_code {
                                Some(value) => value,
                                None => {
                                    *error =
                                        Some("Authorization code cannot be empty.".to_string());
                                    return OverlayUpdate::stay();
                                }
                            }
                        }
                        _ => code.to_string(),
                    };

                    *self = LoginState::Exchanging { provider };

                    OverlayUpdate::stay()
                        .with_effects(vec![OverlayEffect::StartTokenExchange {
                            provider,
                            code,
                            verifier: pkce_verifier,
                            redirect_uri,
                        }])
                        .with_mutations(vec![StateMutation::Auth(
                            AuthMutation::SetCallbackInProgress(false),
                        )])
                }
                KeyCode::Backspace => {
                    input.pop();
                    OverlayUpdate::stay()
                }
                KeyCode::Char(c) if !ctrl => {
                    input.push(c);
                    OverlayUpdate::stay()
                }
                _ => OverlayUpdate::stay(),
            },
            LoginState::Exchanging { .. } => {
                if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
                    OverlayUpdate::close().with_mutations(vec![
                        StateMutation::Auth(AuthMutation::CancelLoginRequest),
                        StateMutation::Auth(AuthMutation::SetCallbackInProgress(false)),
                    ])
                } else {
                    OverlayUpdate::stay()
                }
            }
            LoginState::ApiKeyInfo { .. } => OverlayUpdate::close(),
        }
    }
}
