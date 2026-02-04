//! Auth feature view.
//!
//! Rendering functions for the login overlay.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use zdx_core::providers::oauth::{claude_cli, gemini_cli, openai_codex};

use crate::overlays::LoginState;

/// Renders the login overlay.
pub fn render_login_overlay(frame: &mut Frame, login_state: &LoginState, area: Rect) {
    use crate::overlays::render_utils::{calculate_overlay_area, render_overlay_container};

    let popup_width = 60;
    let popup_height = 12;
    let popup_area = calculate_overlay_area(area, area.height, popup_width, popup_height);

    let title = match login_state.selected_provider() {
        None => "Choose Login Provider",
        Some(provider) => match provider {
            zdx_core::providers::ProviderKind::Anthropic => "Anthropic API Key",
            zdx_core::providers::ProviderKind::ClaudeCli => "Claude CLI Login",
            zdx_core::providers::ProviderKind::OpenAICodex => "OpenAI Codex Login",
            zdx_core::providers::ProviderKind::OpenAI => "OpenAI Login",
            zdx_core::providers::ProviderKind::OpenRouter => "OpenRouter Login",
            zdx_core::providers::ProviderKind::Mimo => "MiMo API Key",
            zdx_core::providers::ProviderKind::Mistral => "Mistral API Key",
            zdx_core::providers::ProviderKind::Moonshot => "Moonshot API Key",
            zdx_core::providers::ProviderKind::Stepfun => "StepFun API Key",
            zdx_core::providers::ProviderKind::Gemini => "Gemini Login",
            zdx_core::providers::ProviderKind::GeminiCli => "Gemini CLI Login",
        },
    };
    render_overlay_container(frame, popup_area, title, Color::Cyan);

    let inner = Rect::new(
        popup_area.x + 2,
        popup_area.y + 1,
        popup_area.width.saturating_sub(4),
        popup_area.height.saturating_sub(2),
    );

    let lines: Vec<Line> = match login_state {
        LoginState::SelectProvider { selected } => {
            let entries = render_cli_provider_entries(inner.width, *selected);
            let mut l = vec![
                Line::from(Span::styled(
                    "Select a CLI provider to log in:",
                    Style::default().fg(Color::White),
                )),
                Line::from(""),
            ];
            l.extend(entries);
            l.push(Line::from(""));
            l.push(Line::from(Span::styled(
                "Enter to continue, Esc to cancel",
                Style::default().fg(Color::DarkGray),
            )));
            l
        }
        LoginState::AwaitingCode { url, error, .. } => {
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
                    "Waiting for browser login callback...",
                    Style::default().fg(Color::White),
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
        LoginState::Exchanging { .. } => vec![
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
        LoginState::ApiKeyInfo { env_var, .. } => vec![
            Line::from(Span::styled(
                "This provider uses API keys.",
                Style::default().fg(Color::Green),
            )),
            Line::from(""),
            Line::from(Span::styled(
                format!("Set {} in your shell.", env_var),
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Press Esc to close",
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

fn render_cli_provider_entries(width: u16, selected: usize) -> Vec<Line<'static>> {
    let label_style = Style::default().fg(Color::White);
    let selected_style = Style::default().fg(Color::Cyan);
    let status_on = Style::default().fg(Color::Green);
    let pad = " ".repeat(2);

    type LoadFn = fn() -> anyhow::Result<Option<zdx_core::providers::oauth::OAuthCredentials>>;

    let providers: [(&str, LoadFn); 3] = [
        ("Claude CLI", claude_cli::load_credentials),
        ("OpenAI Codex", openai_codex::load_credentials),
        ("Gemini CLI", gemini_cli::load_credentials),
    ];

    providers
        .iter()
        .enumerate()
        .map(|(idx, (label, load_fn))| {
            let logged_in = load_fn()
                .ok()
                .flatten()
                .filter(|creds| !creds.is_expired())
                .is_some();
            let status = if logged_in { "✓ logged in" } else { "" };
            let status_style = if logged_in { status_on } else { label_style };
            let pointer = if idx == selected { ">" } else { " " };
            let name_style = if idx == selected {
                selected_style
            } else {
                label_style
            };
            let name = format!("{} {}", pointer, label);
            let spacing = width
                .saturating_sub(name.len() as u16)
                .saturating_sub(status.len() as u16)
                .saturating_sub(2) as usize;
            let mut spans = vec![
                Span::styled(pad.clone(), label_style),
                Span::styled(name, name_style),
                Span::styled(" ".repeat(spacing), label_style),
            ];
            if !status.is_empty() {
                spans.push(Span::styled(status, status_style));
            }
            Line::from(spans)
        })
        .collect()
}
