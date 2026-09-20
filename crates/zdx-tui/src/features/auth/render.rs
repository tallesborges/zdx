//! Auth feature view.
//!
//! Rendering functions for the login overlay.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use zdx_engine::providers::oauth::{
    claude_cli, google_antigravity, grok_build, muse_code, openai_codex,
};

use crate::overlays::LoginState;

type LoadFn =
    fn(Option<&str>) -> anyhow::Result<Option<zdx_engine::providers::oauth::OAuthCredentials>>;

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
            zdx_engine::providers::ProviderKind::Anthropic => "Anthropic API Key",
            zdx_engine::providers::ProviderKind::ClaudeCli => "Claude CLI Login",
            zdx_engine::providers::ProviderKind::OpenAICodex => "OpenAI Codex Login",
            zdx_engine::providers::ProviderKind::OpenAI => "OpenAI Login",
            zdx_engine::providers::ProviderKind::OpenRouter => "OpenRouter Login",
            zdx_engine::providers::ProviderKind::DeepSeek => "DeepSeek API Key",
            zdx_engine::providers::ProviderKind::Xiaomi => "Xiaomi MiMo API Key",
            zdx_engine::providers::ProviderKind::XiaomiPlan => "Xiaomi MiMo Plan API Key",
            zdx_engine::providers::ProviderKind::Mistral => "Mistral API Key",
            zdx_engine::providers::ProviderKind::Moonshot => "Moonshot API Key",
            zdx_engine::providers::ProviderKind::Stepfun => "StepFun API Key",
            zdx_engine::providers::ProviderKind::LMStudio => "LMStudio (Local)",
            zdx_engine::providers::ProviderKind::Gemini => "Gemini Login",
            zdx_engine::providers::ProviderKind::GoogleAntigravity => "Google Antigravity Login",
            zdx_engine::providers::ProviderKind::OpencodeGo => "OpenCode Go API Key",
            zdx_engine::providers::ProviderKind::Minimax => "MiniMax API Key",
            zdx_engine::providers::ProviderKind::Zai => "Z.AI API Key",
            zdx_engine::providers::ProviderKind::Xai => "xAI API Key",
            zdx_engine::providers::ProviderKind::GrokBuild => "Grok Build Login",
            zdx_engine::providers::ProviderKind::Meta => "Meta API Key",
            zdx_engine::providers::ProviderKind::MuseCode => "Muse Code Device Login",
            zdx_engine::providers::ProviderKind::ElevenLabs => "ElevenLabs API Key",
            zdx_engine::providers::ProviderKind::Alibaba => "Alibaba API Key",
            zdx_engine::providers::ProviderKind::QwenCode => "Qwen Code API Key",
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
        LoginState::DeviceStarting { error, .. } => {
            let mut lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Requesting device code...",
                    Style::default().fg(Color::Yellow),
                )),
            ];
            if let Some(error) = error {
                lines.push(Line::from(""));
                lines.extend(error_lines(error, inner_width));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Esc to cancel",
                Style::default().fg(Color::DarkGray),
            )));
            lines
        }
        LoginState::DeviceAwaitingApproval {
            user_code,
            url,
            error,
            ..
        } => render_device_approval_lines(user_code, url, error.as_deref(), inner_width),
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

/// Device-code screen: the user code is the thing to read off, so it gets its
/// own emphasized line rather than being folded into the URL.
fn render_device_approval_lines(
    user_code: &str,
    url: &str,
    error: Option<&str>,
    inner_width: u16,
) -> Vec<Line<'static>> {
    let display_url = truncate_middle(url, inner_width.saturating_sub(2) as usize);
    let mut lines = vec![
        Line::from(Span::styled(
            "Enter this code in your browser:",
            Style::default().fg(Color::Green),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("    {user_code}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            display_url,
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Waiting for approval...",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "Unsupported by Meta; billing attribution unverified.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    if let Some(error) = error {
        lines.push(Line::from(""));
        lines.extend(error_lines(error, inner_width));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Esc to cancel",
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
        Line::from(Span::styled(
            "or paste the code / redirect URL here.",
            Style::default().fg(Color::DarkGray),
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

/// Wraps text to `width`, breaking on whitespace where possible.
///
/// Login errors carry the provider's own explanation; rendering them as one
/// clipped line hides exactly the part that says what went wrong.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(20);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
        // A single token longer than the line (a URL, say) still has to land.
        while current.chars().count() > width {
            let head: String = current.chars().take(width).collect();
            let tail: String = current.chars().skip(width).collect();
            lines.push(head);
            current = tail;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Renders an error as wrapped red lines.
fn error_lines(error: &str, inner_width: u16) -> Vec<Line<'static>> {
    wrap_text(error, inner_width.saturating_sub(2) as usize)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Style::default().fg(Color::Red))))
        .collect()
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

    let providers: [(&str, &str, LoadFn); 5] = [
        (
            "Claude CLI",
            claude_cli::PROVIDER_KEY,
            claude_cli::load_credentials,
        ),
        (
            "OpenAI Codex",
            openai_codex::PROVIDER_KEY,
            openai_codex::load_credentials,
        ),
        (
            "Google Antigravity",
            google_antigravity::PROVIDER_KEY,
            google_antigravity::load_credentials,
        ),
        (
            "Grok Build",
            grok_build::PROVIDER_KEY,
            grok_build::load_credentials,
        ),
        (
            "Muse Code",
            muse_code::PROVIDER_KEY,
            muse_code::load_credentials,
        ),
    ];
    let cache = zdx_engine::providers::oauth::OAuthCache::load().unwrap_or_default();

    providers
        .iter()
        .enumerate()
        .map(|(idx, (label, provider_key, load_fn))| {
            let logged_in = load_fn(None)
                .ok()
                .flatten()
                .as_ref()
                .is_some_and(|creds| !creds.is_expired());
            let named = cache
                .accounts(provider_key)
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            let status = if named.is_empty() {
                if logged_in { "✓ logged in" } else { "" }.to_string()
            } else if logged_in {
                format!("✓ logged in · +{}", named.join(", "))
            } else {
                format!("✓ {}", named.join(", "))
            };
            let logged_in = logged_in || !named.is_empty();
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
                spans.push(Span::styled(status.clone(), status_style));
            }
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::wrap_text;

    /// A provider error is the only thing that says why a login failed, so it
    /// must survive rendering instead of being clipped at the overlay edge.
    #[test]
    fn long_errors_wrap_instead_of_being_cut_off() {
        let error = "Muse Code device authorization failed (HTTP 302 Found); the request \
                     was redirected away from the API, which usually means Meta rejected \
                     the client";
        let lines = wrap_text(error, 40);

        assert!(lines.len() > 1, "expected wrapping, got {lines:?}");
        assert!(lines.iter().all(|l| l.chars().count() <= 40));
        // Every word survives, including the tail that a clipped line loses.
        let rejoined = lines.join(" ");
        for word in ["302", "redirected", "rejected", "client"] {
            assert!(rejoined.contains(word), "lost {word:?} in {lines:?}");
        }
    }

    #[test]
    fn unbroken_tokens_are_split_rather_than_overflowing() {
        let lines = wrap_text(&"x".repeat(95), 30);
        assert!(lines.len() >= 3);
        assert!(lines.iter().all(|l| l.chars().count() <= 30));
        assert_eq!(lines.concat().chars().count(), 95);
    }

    #[test]
    fn short_errors_stay_on_one_line() {
        assert_eq!(
            wrap_text("Meta did not issue a key", 60),
            vec!["Meta did not issue a key".to_string()]
        );
    }
}
