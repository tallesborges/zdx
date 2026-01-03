//! Login overlay.
//!
//! Contains state, update handlers, and render function for the OAuth login flow.
//!
//! ## Flow
//!
//! 1. User runs `/login` command → `UiEffect::OpenLogin`
//! 2. Runtime calls `open_login()` → opens overlay, returns `OpenBrowser` effect
//! 3. User pastes auth code → `handle_key()` spawns token exchange
//! 4. Async result arrives → `handle_login_result()` closes overlay or shows error

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::{Overlay, OverlayAction, OverlayState};
use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::state::TuiState;
use crate::ui::transcript::HistoryCell;

// ============================================================================
// State
// ============================================================================

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
// Overlay Trait Implementation
// ============================================================================

impl Overlay for LoginState {
    type Config = ();

    fn open(_: Self::Config) -> (Self, Vec<UiEffect>) {
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

    fn render(&self, frame: &mut Frame, area: Rect, _input_y: u16) {
        // Login overlay centers in full area, doesn't use input_y
        render_login_overlay(frame, self, area)
    }

    fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match self {
            LoginState::AwaitingCode { input, .. } => match key.code {
                KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                    tui.auth.login_rx = None;
                    Some(OverlayAction::close())
                }
                KeyCode::Enter => {
                    let code = input.trim().to_string();
                    if code.is_empty() {
                        None
                    } else {
                        // Get the verifier before transitioning
                        let verifier = match self {
                            LoginState::AwaitingCode { pkce_verifier, .. } => pkce_verifier.clone(),
                            _ => return None,
                        };

                        // Transition to Exchanging state
                        Some(OverlayAction::Transition {
                            new_state: OverlayState::Login(LoginState::Exchanging),
                            effects: vec![UiEffect::SpawnTokenExchange { code, verifier }],
                        })
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    None
                }
                KeyCode::Char(c) if !ctrl => {
                    input.push(c);
                    None
                }
                _ => None,
            },
            LoginState::Exchanging => {
                if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
                    tui.auth.login_rx = None;
                    Some(OverlayAction::close())
                } else {
                    None
                }
            }
        }
    }
}

// ============================================================================
// Async Result Handler
// ============================================================================

/// Handles the result of an async token exchange.
pub fn handle_login_result(
    tui: &mut TuiState,
    overlay: &mut OverlayState,
    result: Result<(), String>,
) {
    use crate::providers::oauth::anthropic;

    tui.auth.login_rx = None;
    match result {
        Ok(()) => {
            *overlay = OverlayState::None;
            tui.refresh_auth_type();
            tui.transcript
                .cells
                .push(HistoryCell::system("Logged in with Anthropic OAuth."));
        }
        Err(msg) => {
            // Generate new PKCE for retry
            let pkce = anthropic::generate_pkce();
            let url = anthropic::build_auth_url(&pkce);
            *overlay = OverlayState::Login(LoginState::AwaitingCode {
                url,
                pkce_verifier: pkce.verifier,
                input: String::new(),
                error: Some(msg),
            });
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

            // Show different message based on whether this is initial or retry
            let status_message = if error.is_some() {
                "Visit URL to retry authentication:"
            } else {
                "Browser opened for authentication."
            };
            let status_color = if error.is_some() {
                Color::Yellow
            } else {
                Color::Green
            };

            let mut l = vec![
                Line::from(Span::styled(
                    status_message,
                    Style::default().fg(status_color),
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
