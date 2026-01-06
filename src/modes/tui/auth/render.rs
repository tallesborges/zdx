//! Auth feature view.
//!
//! Rendering functions for the login overlay.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::modes::tui::overlays::LoginState;

/// Renders the login overlay.
pub fn render_login_overlay(frame: &mut Frame, login_state: &LoginState, area: Rect) {
    use crate::modes::tui::overlays::render_utils::{
        calculate_overlay_area, render_overlay_container,
    };

    let popup_width = 60;
    let popup_height = 9;
    let popup_area = calculate_overlay_area(area, area.height, popup_width, popup_height);

    render_overlay_container(frame, popup_area, "Anthropic Login", Color::Cyan);

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

/// Truncates a string in the middle with "..." if it exceeds max_len.
fn truncate_middle(s: &str, max_len: usize) -> String {
    if s.len() <= max_len || max_len < 10 {
        return s.to_string();
    }
    let half = (max_len - 3) / 2;
    format!("{}...{}", &s[..half], &s[s.len() - half..])
}
