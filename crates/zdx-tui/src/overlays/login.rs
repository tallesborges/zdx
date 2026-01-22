use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use zdx_core::providers::ProviderKind;

use super::OverlayUpdate;
use crate::auth::render_login_overlay;
use crate::common::TaskKind;
use crate::effects::UiEffect;
use crate::state::TuiState;

#[derive(Debug, Clone)]
pub enum LoginState {
    SelectProvider {
        selected: usize,
    },
    AwaitingCode {
        provider: ProviderKind,
        url: String,
        pkce_verifier: String,
        oauth_state: Option<String>,
        redirect_uri: Option<String>,
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
    pub fn open(_tui: &TuiState) -> (Self, Vec<UiEffect>) {
        (LoginState::SelectProvider { selected: 0 }, vec![])
    }

    fn cli_providers() -> &'static [ProviderKind] {
        &[
            ProviderKind::ClaudeCli,
            ProviderKind::OpenAICodex,
            ProviderKind::GeminiCli,
        ]
    }

    pub fn reopen(provider: ProviderKind, error: String) -> Self {
        let (state, _) = Self::open_with_provider(provider, Some(error));
        state
    }

    fn open_with_provider(provider: ProviderKind, error: Option<String>) -> (Self, Vec<UiEffect>) {
        match provider {
            ProviderKind::Anthropic => {
                let env_var = provider.api_key_env_var().unwrap_or("API_KEY").to_string();
                (LoginState::ApiKeyInfo { provider, env_var }, vec![])
            }
            ProviderKind::ClaudeCli => {
                use zdx_core::providers::oauth::claude_cli;

                let pkce = claude_cli::generate_pkce();
                let oauth_state = uuid::Uuid::new_v4().to_string();
                let callback_port = claude_cli::random_local_port();
                let redirect_uri = claude_cli::build_redirect_uri(callback_port);
                let url = claude_cli::build_auth_url(&pkce, &oauth_state, &redirect_uri);
                let state = LoginState::AwaitingCode {
                    provider,
                    url: url.clone(),
                    pkce_verifier: pkce.verifier,
                    oauth_state: Some(oauth_state.clone()),
                    redirect_uri: Some(redirect_uri),
                    error,
                };
                let effects = vec![
                    UiEffect::OpenBrowser { url },
                    UiEffect::StartLocalAuthCallback {
                        task: None,
                        provider,
                        state: Some(oauth_state),
                        port: Some(callback_port),
                    },
                ];
                (state, effects)
            }
            ProviderKind::OpenAICodex => {
                use zdx_core::providers::oauth::openai_codex;

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
                    error,
                };
                let effects = vec![
                    UiEffect::OpenBrowser { url },
                    UiEffect::StartLocalAuthCallback {
                        task: None,
                        provider,
                        state: Some(oauth_state_copy),
                        port: None,
                    },
                ];
                (state, effects)
            }
            ProviderKind::GeminiCli => {
                use zdx_core::providers::oauth::gemini_cli;

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
                    error,
                };
                let effects = vec![
                    UiEffect::OpenBrowser { url },
                    UiEffect::StartLocalAuthCallback {
                        task: None,
                        provider,
                        state: Some(oauth_state_copy),
                        port: None,
                    },
                ];
                (state, effects)
            }
            _ => {
                let env_var = provider.api_key_env_var().unwrap_or("API_KEY").to_string();
                (LoginState::ApiKeyInfo { provider, env_var }, vec![])
            }
        }
    }

    pub fn selected_provider(&self) -> Option<ProviderKind> {
        match self {
            LoginState::SelectProvider { .. } => None,
            LoginState::AwaitingCode { provider, .. } => Some(*provider),
            LoginState::Exchanging { provider } => Some(*provider),
            LoginState::ApiKeyInfo { provider, .. } => Some(*provider),
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, _input_y: u16) {
        render_login_overlay(frame, self, area)
    }

    pub fn handle_key(&mut self, _tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match self {
            LoginState::SelectProvider { selected } => match key.code {
                KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                    OverlayUpdate::close()
                }
                KeyCode::Up => {
                    if *selected > 0 {
                        *selected -= 1;
                    }
                    OverlayUpdate::stay()
                }
                KeyCode::Down => {
                    if *selected < Self::cli_providers().len().saturating_sub(1) {
                        *selected += 1;
                    }
                    OverlayUpdate::stay()
                }
                KeyCode::Enter => {
                    let provider = Self::cli_providers()
                        .get(*selected)
                        .copied()
                        .unwrap_or(ProviderKind::OpenAICodex);
                    let (state, effects) = Self::open_with_provider(provider, None);
                    *self = state;
                    OverlayUpdate::stay().with_ui_effects(effects)
                }
                _ => OverlayUpdate::stay(),
            },
            LoginState::AwaitingCode { .. } => match key.code {
                KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                    OverlayUpdate::close().with_ui_effects(vec![
                        UiEffect::CancelTask {
                            kind: TaskKind::LoginExchange,
                            token: None,
                        },
                        UiEffect::CancelTask {
                            kind: TaskKind::LoginCallback,
                            token: None,
                        },
                    ])
                }
                _ => OverlayUpdate::stay(),
            },
            LoginState::Exchanging { .. } => {
                if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
                    OverlayUpdate::close().with_ui_effects(vec![
                        UiEffect::CancelTask {
                            kind: TaskKind::LoginExchange,
                            token: None,
                        },
                        UiEffect::CancelTask {
                            kind: TaskKind::LoginCallback,
                            token: None,
                        },
                    ])
                } else {
                    OverlayUpdate::stay()
                }
            }
            LoginState::ApiKeyInfo { .. } => OverlayUpdate::close(),
        }
    }
}
