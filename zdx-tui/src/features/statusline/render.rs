//! Status line rendering.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::state::StatusLine;

/// Renders the debug status line (just FPS).
pub fn render_debug_status_line(status: &StatusLine, frame: &mut Frame, area: Rect) {
    let fps_style = if status.fps < 30.0 {
        Style::default().fg(Color::Red)
    } else if status.fps < 55.0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    };

    let line = Line::from(Span::styled(format!("{:.1}fps", status.fps), fps_style));
    frame.render_widget(Paragraph::new(line), area);
}
