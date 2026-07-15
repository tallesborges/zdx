//! Pure conversion from semantic `StyledLine`s to ratatui `Line`s, plus a
//! stateless `cells_to_lines` helper for non-interactive consumers (e.g. the
//! monitor transcript overlay) that don't need selection or lazy rendering.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::cell::HistoryCell;
use crate::style::{Style as TranscriptStyle, StyledLine};
use crate::text::ratatui_text;

/// Converts a transcript `StyledLine` to a ratatui `Line`.
pub fn convert_styled_line(styled_line: &StyledLine) -> Line<'static> {
    let spans: Vec<Span<'static>> = styled_line
        .spans
        .iter()
        .map(|s| {
            let style = convert_style(s.style);
            Span::styled(ratatui_text(&s.text).into_owned(), style)
        })
        .collect();
    Line::from(spans)
}

/// Renders a slice of transcript cells into ratatui lines, inserting one blank
/// line between cells. Intended for static/persisted transcripts, so any
/// in-progress cell renders at spinner frame 0.
pub fn cells_to_lines(cells: &[HistoryCell], width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for cell in cells {
        for styled in cell.display_lines(width, 0) {
            lines.push(convert_styled_line(&styled));
        }
        lines.push(Line::default());
    }
    lines
}

/// Converts a semantic transcript `Style` to a ratatui `Style`.
pub fn convert_style(style: TranscriptStyle) -> Style {
    match style {
        TranscriptStyle::Plain => Style::default(),
        TranscriptStyle::UserPrefix | TranscriptStyle::ToolSuccess => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        TranscriptStyle::User | TranscriptStyle::BlockQuote => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::ITALIC),
        TranscriptStyle::Assistant => Style::default().fg(Color::White),
        TranscriptStyle::StreamingCursor => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::SLOW_BLINK),
        TranscriptStyle::SystemPrefix => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        TranscriptStyle::System | TranscriptStyle::ToolOutput | TranscriptStyle::CodeFence => {
            Style::default().fg(Color::DarkGray)
        }
        TranscriptStyle::ToolStatus => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        TranscriptStyle::ToolError => Style::default().fg(Color::Red),
        TranscriptStyle::ToolRunning | TranscriptStyle::CodeInline | TranscriptStyle::CodeBlock => {
            Style::default().fg(Color::Cyan)
        }
        TranscriptStyle::ToolCancelled => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::CROSSED_OUT | Modifier::BOLD),
        TranscriptStyle::ToolTruncation | TranscriptStyle::ToolBracket => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::DIM),
        TranscriptStyle::ThinkingPrefix => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::DIM),
        TranscriptStyle::Thinking | TranscriptStyle::Timing => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM | Modifier::ITALIC),
        TranscriptStyle::Interrupted | TranscriptStyle::TableBorder => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),

        // Markdown styles
        TranscriptStyle::Emphasis => Style::default().add_modifier(Modifier::ITALIC),
        TranscriptStyle::Strong | TranscriptStyle::H2 => {
            Style::default().add_modifier(Modifier::BOLD)
        }
        TranscriptStyle::H1 => Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        TranscriptStyle::H3 => Style::default()
            .add_modifier(Modifier::ITALIC)
            .fg(Color::White),
        TranscriptStyle::Link => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::UNDERLINED),
        TranscriptStyle::ListBullet | TranscriptStyle::ListNumber => {
            Style::default().fg(Color::Yellow)
        }
        TranscriptStyle::ImagePlaceholder => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    }
}
