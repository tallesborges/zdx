use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::{Overlay, OverlayAction};
use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::state::TuiState;
use crate::ui::transcript::HistoryCell;

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

    pub fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction> {
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
                        return None;
                    }

                    let verifier = match self {
                        LoginState::AwaitingCode { pkce_verifier, .. } => pkce_verifier.clone(),
                        _ => return None,
                    };

                    *self = LoginState::Exchanging;

                    Some(OverlayAction::Effects(vec![UiEffect::SpawnTokenExchange {
                        code,
                        verifier,
                    }]))
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

fn truncate_middle(s: &str, max_len: usize) -> String {
    if s.len() <= max_len || max_len < 10 {
        return s.to_string();
    }
    let half = (max_len - 3) / 2;
    format!("{}...{}", &s[..half], &s[s.len() - half..])
}
