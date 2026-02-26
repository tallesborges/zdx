use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::common::truncate_start_with_ellipsis;

/// Calculates the area for an overlay, centered horizontally and vertically
/// within the available height (usually above the input bar).
pub fn calculate_overlay_area(area: Rect, available_height: u16, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(available_height.saturating_sub(2));

    let overlay_x = (area.width.saturating_sub(width)) / 2;
    let overlay_y = (available_height.saturating_sub(height)) / 2;
    Rect::new(overlay_x, overlay_y, width, height)
}

/// Renders the base container for an overlay (clears background, draws border and title).
pub fn render_overlay_container(frame: &mut Frame, area: Rect, title: &str, border_color: Color) {
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(format!(" {title} "))
        .title_style(
            Style::default()
                .fg(border_color)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(block, area);
}

/// Input configuration for an overlay.
pub struct OverlayConfig<'a> {
    pub title: &'a str,
    pub border_color: Color,
    pub width: u16,
    pub height: u16,
    pub hints: &'a [InputHint<'a>],
}

/// Layout rectangles for an overlay.
pub struct OverlayLayout {
    pub popup: Rect,
    pub inner: Rect,
    pub body: Rect,
    pub footer: Rect,
}

/// Render a standard overlay container and return its layout.
pub fn render_overlay(
    frame: &mut Frame,
    area: Rect,
    input_y: u16,
    config: &OverlayConfig<'_>,
) -> OverlayLayout {
    let popup = calculate_overlay_area(area, input_y, config.width, config.height);
    render_overlay_container(frame, popup, config.title, config.border_color);

    let inner = Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );

    if !config.hints.is_empty() {
        render_hints(frame, inner, config.hints, config.border_color);
    }

    let footer_height = u16::from(!config.hints.is_empty());
    let body_height = inner.height.saturating_sub(footer_height);
    let footer = Rect::new(inner.x, inner.y + body_height, inner.width, footer_height);
    let body = Rect::new(inner.x, inner.y, inner.width, body_height);

    OverlayLayout {
        popup,
        inner,
        body,
        footer,
    }
}

/// Helper struct for keyboard hints.
pub struct InputHint<'a> {
    pub key: &'a str,
    pub action: &'a str,
}

impl<'a> InputHint<'a> {
    pub fn new(key: &'a str, action: &'a str) -> Self {
        Self { key, action }
    }
}

/// Configuration for rendering a prompt input line (e.g., filter or rename input).
pub struct InputLine<'a> {
    pub value: &'a str,
    pub placeholder: Option<&'a str>,
    pub prompt: &'a str,
    pub prompt_color: Color,
    pub text_color: Color,
    pub placeholder_color: Color,
    pub cursor_color: Color,
}

/// Renders a prompt-style input line: "> <text>█".
pub fn render_input_line(frame: &mut Frame, area: Rect, input: &InputLine<'_>) {
    let is_placeholder = input.value.is_empty() && input.placeholder.is_some();
    let max_text_width = area.width.saturating_sub(input.prompt.len() as u16 + 1) as usize;

    let display_text = if is_placeholder {
        truncate_start_with_ellipsis(input.placeholder.unwrap_or(""), max_text_width)
    } else if input.value.is_empty() {
        String::new()
    } else {
        truncate_start_with_ellipsis(input.value, max_text_width)
    };

    let mut spans = Vec::new();
    spans.push(Span::styled(
        input.prompt,
        Style::default().fg(input.prompt_color),
    ));

    if is_placeholder {
        spans.push(Span::styled("█", Style::default().fg(input.cursor_color)));
        if !display_text.is_empty() {
            spans.push(Span::styled(
                display_text,
                Style::default().fg(input.placeholder_color),
            ));
        }
    } else {
        spans.push(Span::styled(
            display_text,
            Style::default().fg(input.text_color),
        ));
        spans.push(Span::styled("█", Style::default().fg(input.cursor_color)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Renders a line of keyboard hints at the bottom of the overlay.
pub fn render_hints(frame: &mut Frame, area: Rect, hints: &[InputHint], highlight_color: Color) {
    let hints_y = area.y + area.height.saturating_sub(1);
    let hints_area = Rect::new(area.x, hints_y, area.width, 1);

    let mut spans = Vec::new();
    for (i, hint) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" • ", Style::default().fg(Color::DarkGray)));
        }
        spans.push(Span::styled(hint.key, Style::default().fg(highlight_color)));
        spans.push(Span::styled(
            format!(" {}", hint.action),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let line = Line::from(spans);
    let para = Paragraph::new(line).alignment(Alignment::Center);
    frame.render_widget(para, hints_area);
}

/// Renders a separator line.
pub fn render_separator(frame: &mut Frame, area: Rect, y_offset: u16) {
    if y_offset >= area.height {
        return;
    }
    let separator = "─".repeat(area.width as usize);
    let separator_area = Rect::new(area.x, area.y + y_offset, area.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            separator,
            Style::default().fg(Color::DarkGray),
        ))),
        separator_area,
    );
}

/// Returns a centered rectangle of the given percentage size within `r`.
pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    use ratatui::layout::{Constraint, Direction, Layout};
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
