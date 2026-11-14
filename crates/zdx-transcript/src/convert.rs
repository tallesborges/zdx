//! Pure conversion from semantic `StyledLine`s to ratatui `Line`s, plus a
//! stateless helper for consumers that hard-wrap ratatui lines themselves
//! (the chat TUI's tool popup).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use crate::style::{Style as TranscriptStyle, StyledLine};
use crate::text::{ratatui_text, ratatui_width};

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

/// Hard-wraps a ratatui line to `width` display columns, preserving span styles
/// and grapheme clusters. Consecutive graphemes sharing a style stay in one span.
pub fn wrap_line_to_width(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line.clone()];
    }

    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut row_width = 0usize;
    let mut seg_text = String::new();
    let mut seg_style = Style::default();

    for span in &line.spans {
        for grapheme in span.content.graphemes(true) {
            let grapheme_width = ratatui_width(grapheme);
            if row_width + grapheme_width > width && row_width > 0 {
                if !seg_text.is_empty() {
                    row.push(Span::styled(std::mem::take(&mut seg_text), seg_style));
                }
                rows.push(Line::from(std::mem::take(&mut row)));
                row_width = 0;
            }
            if seg_style != span.style && !seg_text.is_empty() {
                row.push(Span::styled(std::mem::take(&mut seg_text), seg_style));
            }
            seg_style = span.style;
            seg_text.push_str(grapheme);
            row_width += grapheme_width;
        }
    }

    if !seg_text.is_empty() {
        row.push(Span::styled(seg_text, seg_style));
    }
    rows.push(Line::from(row));
    rows
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row_texts(rows: &[Line<'static>]) -> Vec<String> {
        rows.iter()
            .map(|row| row.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn wrap_line_hard_wraps_by_display_width() {
        let rows = wrap_line_to_width(&Line::from("abcdef"), 3);
        assert_eq!(row_texts(&rows), vec!["abc", "def"]);
    }

    #[test]
    fn wrap_line_preserves_empty_line() {
        let rows = wrap_line_to_width(&Line::from(""), 10);
        assert_eq!(row_texts(&rows), vec![String::new()]);
    }

    #[test]
    fn wrap_line_keeps_full_text_across_rows() {
        // Wide graphemes count as their display width; text must round-trip.
        let rows = wrap_line_to_width(&Line::from("a你b好c"), 3);
        assert_eq!(row_texts(&rows).concat(), "a你b好c");
    }
}
