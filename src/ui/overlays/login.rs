//! Login overlay.
//!
//! Contains state, update handlers, and render function for the OAuth login flow.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::ui::effects::UiEffect;
use crate::ui::state::{OverlayState, TuiState};
use crate::ui::transcript::HistoryCell;

// ============================================================================
// State
// ============================================================================

/// Events for the login flow (reducer pattern).
///
/// These events drive the login overlay state machine.
#[derive(Debug, Clone)]
pub enum LoginEvent {
    /// User requested login (e.g., via `/login` command).
    LoginRequested,
    /// User entered the auth code.
    AuthCodeEntered { code: String },
    /// Login succeeded.
    LoginSucceeded,
    /// Login failed with an error message.
    LoginFailed { message: String },
    /// User cancelled the login flow.
    LoginCancelled,
}

/// State for the login overlay.
#[derive(Debug, Clone)]
pub enum LoginState {
    /// Showing auth URL, waiting for user to paste code.
    AwaitingCode {
        /// The auth URL to display.
        url: String,
        /// PKCE verifier for code exchange.
        pkce_verifier: String,
        /// User's input (the auth code).
        input: String,
        /// Error message from previous attempt (if any).
        error: Option<String>,
    },
    /// Exchanging code for tokens (async operation in progress).
    /// The code and verifier are passed to the effect, not stored here.
    Exchanging,
}

// ============================================================================
// Update Handlers
// ============================================================================

/// Main login state machine update function.
pub fn update_login(state: &mut TuiState, event: LoginEvent) -> Vec<UiEffect> {
    use crate::providers::oauth::anthropic;

    match event {
        LoginEvent::LoginRequested => {
            let pkce = anthropic::generate_pkce();
            let url = anthropic::build_auth_url(&pkce);
            state.overlay = OverlayState::Login(LoginState::AwaitingCode {
                url: url.clone(),
                pkce_verifier: pkce.verifier,
                input: String::new(),
                error: None,
            });
            vec![UiEffect::OpenBrowser { url }]
        }
        LoginEvent::AuthCodeEntered { code } => {
            if let OverlayState::Login(LoginState::AwaitingCode { pkce_verifier, .. }) =
                &state.overlay
            {
                let verifier = pkce_verifier.clone();
                state.overlay = OverlayState::Login(LoginState::Exchanging);
                vec![UiEffect::SpawnTokenExchange { code, verifier }]
            } else {
                vec![]
            }
        }
        LoginEvent::LoginSucceeded => {
            state.overlay = OverlayState::None;
            state.refresh_auth_type();
            state
                .transcript
                .push(HistoryCell::system("Logged in with Anthropic OAuth."));
            vec![]
        }
        LoginEvent::LoginFailed { message } => {
            let pkce = anthropic::generate_pkce();
            let url = anthropic::build_auth_url(&pkce);
            state.overlay = OverlayState::Login(LoginState::AwaitingCode {
                url,
                pkce_verifier: pkce.verifier,
                input: String::new(),
                error: Some(message),
            });
            vec![]
        }
        LoginEvent::LoginCancelled => {
            state.overlay = OverlayState::None;
            state.login_exchange_rx = None;
            vec![]
        }
    }
}

/// Handles key events for the login overlay.
pub fn handle_login_key(state: &mut TuiState, key: crossterm::event::KeyEvent) -> Vec<UiEffect> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match &mut state.overlay {
        OverlayState::Login(LoginState::AwaitingCode { input, .. }) => match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                update_login(state, LoginEvent::LoginCancelled)
            }
            KeyCode::Enter => {
                let code = input.trim().to_string();
                if !code.is_empty() {
                    update_login(state, LoginEvent::AuthCodeEntered { code })
                } else {
                    vec![]
                }
            }
            KeyCode::Backspace => {
                input.pop();
                vec![]
            }
            KeyCode::Char(c) if !ctrl => {
                input.push(c);
                vec![]
            }
            _ => vec![],
        },
        OverlayState::Login(LoginState::Exchanging) => {
            if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
                update_login(state, LoginEvent::LoginCancelled)
            } else {
                vec![]
            }
        }
        _ => vec![],
    }
}

/// Handles the result of an async token exchange.
pub fn handle_login_result(state: &mut TuiState, result: Result<(), String>) {
    state.login_exchange_rx = None;
    match result {
        Ok(()) => {
            let _ = update_login(state, LoginEvent::LoginSucceeded);
        }
        Err(msg) => {
            let _ = update_login(state, LoginEvent::LoginFailed { message: msg });
        }
    }
}

// ============================================================================
// Render
// ============================================================================

/// Renders the login overlay.
pub fn render_login_overlay(frame: &mut Frame, login_state: &LoginState, area: Rect) {
    let popup_width = 60.min(area.width.saturating_sub(4));
    let popup_height = 9.min(area.height.saturating_sub(4));
    let popup_x = (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Anthropic Login ")
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(block, popup_area);

    let inner = Rect::new(
        popup_area.x + 2,
        popup_area.y + 1,
        popup_area.width.saturating_sub(4),
        popup_area.height.saturating_sub(2),
    );

    let lines: Vec<Line> = match login_state {
        LoginState::AwaitingCode {
            url, input, error, ..
        } => {
            let display_url = truncate_middle(url, inner.width.saturating_sub(2) as usize);

            let mut l = vec![
                Line::from(Span::styled(
                    "Browser opened for authentication.",
                    Style::default().fg(Color::Green),
                )),
                Line::from(Span::styled(
                    display_url,
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Paste auth code:",
                    Style::default().fg(Color::White),
                )),
                Line::from(Span::styled(
                    format!("> {}█", input),
                    Style::default().fg(Color::Yellow),
                )),
            ];
            if let Some(e) = error {
                l.push(Line::from(""));
                l.push(Line::from(Span::styled(
                    e.as_str(),
                    Style::default().fg(Color::Red),
                )));
            }
            l.push(Line::from(""));
            l.push(Line::from(Span::styled(
                "Esc to cancel",
                Style::default().fg(Color::DarkGray),
            )));
            l
        }
        LoginState::Exchanging => vec![
            Line::from(""),
            Line::from(Span::styled(
                "Exchanging code...",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Esc to cancel",
                Style::default().fg(Color::DarkGray),
            )),
        ],
    };

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

/// Truncates a string in the middle with "..." if too long.
fn truncate_middle(s: &str, max_len: usize) -> String {
    if s.len() <= max_len || max_len < 10 {
        return s.to_string();
    }
    let half = (max_len - 3) / 2;
    format!("{}...{}", &s[..half], &s[s.len() - half..])
}
