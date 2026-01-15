use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

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
        .title(format!(" {} ", title))
        .title_style(
            Style::default()
                .fg(border_color)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(block, area);
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
