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

type LoadFn = fn() -> anyhow::Result<Option<zdx_core::providers::oauth::OAuthCredentials>>;

/// Renders the login overlay.
pub fn render_login_overlay(frame: &mut Frame, login_state: &LoginState, area: Rect) {
    use crate::overlays::render_utils::{calculate_overlay_area, render_overlay_container};

    let popup_width = 60;
    let popup_height = 12;
    let popup_area = calculate_overlay_area(area, area.height, popup_width, popup_height);

    let title = login_overlay_title(login_state);
    render_overlay_container(frame, popup_area, title, Color::Cyan);

    let inner = Rect::new(
        popup_area.x + 2,
        popup_area.y + 1,
        popup_area.width.saturating_sub(4),
        popup_area.height.saturating_sub(2),
    );

    let lines = render_login_overlay_lines(login_state, inner.width);

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

fn login_overlay_title(login_state: &LoginState) -> &'static str {
    match login_state.selected_provider() {
        None => "Choose Login Provider",
        Some(provider) => match provider {
            zdx_core::providers::ProviderKind::Anthropic => "Anthropic API Key",
            zdx_core::providers::ProviderKind::ClaudeCli => "Claude CLI Login",
            zdx_core::providers::ProviderKind::OpenAICodex => "OpenAI Codex Login",
            zdx_core::providers::ProviderKind::OpenAI => "OpenAI Login",
            zdx_core::providers::ProviderKind::OpenRouter => "OpenRouter Login",
            zdx_core::providers::ProviderKind::Xiomi => "Xiomi API Key",
            zdx_core::providers::ProviderKind::Mistral => "Mistral API Key",
            zdx_core::providers::ProviderKind::Moonshot => "Moonshot API Key",
            zdx_core::providers::ProviderKind::Stepfun => "StepFun API Key",
            zdx_core::providers::ProviderKind::Gemini => "Gemini Login",
            zdx_core::providers::ProviderKind::GeminiCli => "Gemini CLI Login",
            zdx_core::providers::ProviderKind::Zen => "Zen API Key",
            zdx_core::providers::ProviderKind::Apiyi => "APIYI API Key",
            zdx_core::providers::ProviderKind::Minimax => "MiniMax API Key",
            zdx_core::providers::ProviderKind::Zai => "Z.AI API Key",
            zdx_core::providers::ProviderKind::Xai => "xAI API Key",
        },
    }
}

fn render_login_overlay_lines(login_state: &LoginState, inner_width: u16) -> Vec<Line<'static>> {
    match login_state {
        LoginState::SelectProvider { selected } => {
            render_provider_selection_lines(inner_width, *selected)
        }
        LoginState::AwaitingCode { url, error, .. } => {
            render_awaiting_code_lines(url, error.as_deref(), inner_width)
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
                format!("Set {env_var} in your shell."),
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Press Esc to close",
                Style::default().fg(Color::DarkGray),
            )),
        ],
    }
}

fn render_provider_selection_lines(inner_width: u16, selected: usize) -> Vec<Line<'static>> {
    let entries = render_cli_provider_entries(inner_width, selected);
    let mut lines = vec![
        Line::from(Span::styled(
            "Select a CLI provider to log in:",
            Style::default().fg(Color::White),
        )),
        Line::from(""),
    ];
    lines.extend(entries);
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter to continue, Esc to cancel",
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

fn render_awaiting_code_lines(
    url: &str,
    error: Option<&str>,
    inner_width: u16,
) -> Vec<Line<'static>> {
    let display_url = truncate_middle(url, inner_width.saturating_sub(2) as usize);
    let has_error = error.is_some();
    let status_message = if has_error {
        "Visit URL to retry authentication:"
    } else {
        "Browser opened for authentication."
    };
    let status_color = if has_error {
        Color::Yellow
    } else {
        Color::Green
    };

    let mut lines = vec![
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
    if let Some(error) = error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            error.to_string(),
            Style::default().fg(Color::Red),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Esc to cancel",
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

/// Truncates a string in the middle with "..." if it exceeds `max_len`.
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
            let name = format!("{pointer} {label}");
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
