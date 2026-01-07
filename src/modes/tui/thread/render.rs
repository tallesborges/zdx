//! Thread feature view.
//!
//! Rendering functions for the thread picker overlay.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::core::thread_log::{self, short_thread_id};
use crate::modes::tui::overlays::ThreadPickerState;

const MAX_VISIBLE_THREADS: usize = 10;

/// Renders the thread picker overlay.
pub fn render_thread_picker(
    frame: &mut Frame,
    picker: &ThreadPickerState,
    area: Rect,
    input_top_y: u16,
) {
    use crate::modes::tui::overlays::render_utils::{
        InputHint, calculate_overlay_area, render_hints, render_overlay_container, render_separator,
    };

    let thread_count = picker.threads.len();
    let visible_count = thread_count.min(MAX_VISIBLE_THREADS);

    let picker_width = 60;
    let picker_height = (visible_count as u16 + 5).max(7);

    let picker_area = calculate_overlay_area(area, input_top_y, picker_width, picker_height);
    render_overlay_container(
        frame,
        picker_area,
        &format!("Threads ({})", thread_count),
        Color::Blue,
    );

    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    if picker.threads.is_empty() {
        let empty_msg = Paragraph::new("No threads found")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        frame.render_widget(empty_msg, inner_area);
        return;
    }

    let list_height = inner_area.height.saturating_sub(2) as usize;

    let list_area = Rect::new(
        inner_area.x,
        inner_area.y,
        inner_area.width,
        list_height as u16,
    );

    let items: Vec<ListItem> = picker
        .threads
        .iter()
        .skip(picker.offset)
        .take(list_height)
        .map(|thread| {
            let timestamp = thread
                .modified
                .and_then(thread_log::format_timestamp)
                .unwrap_or_else(|| "unknown".to_string());

            let display_title = truncate_with_ellipsis(
                &thread.display_title(),
                (inner_area.width as usize).saturating_sub(20),
            );
            let short_id = short_thread_id(&thread.id);

            let line = Line::from(vec![
                Span::styled(short_id, Style::default().fg(Color::Cyan)),
                Span::styled("  ", Style::default()),
                Span::styled(display_title, Style::default().fg(Color::White)),
                Span::styled("  ", Style::default()),
                Span::styled(timestamp, Style::default().fg(Color::DarkGray)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    let visible_selected = picker.selected.saturating_sub(picker.offset);
    list_state.select(Some(visible_selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, inner_area, list_height as u16);

    // Show "Copied!" feedback briefly, otherwise show normal hint
    let copy_hint = if picker.should_show_copied() {
        InputHint::new("✓", "Copied!")
    } else {
        InputHint::new("y", "copy id")
    };

    render_hints(
        frame,
        inner_area,
        &[
            InputHint::new("↑↓", "navigate"),
            InputHint::new("Enter", "select"),
            copy_hint,
            InputHint::new("Esc", "cancel"),
        ],
        Color::Blue,
    );
}

/// Truncates a string with ellipsis if it exceeds max_width.
fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let mut truncated = String::new();
    for ch in text.chars() {
        let next_width = truncated.width() + ch.width().unwrap_or(0);
        if next_width + 1 > max_width {
            break;
        }
        truncated.push(ch);
    }
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_with_ellipsis_short() {
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_with_ellipsis_exact() {
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_with_ellipsis_truncated() {
        assert_eq!(truncate_with_ellipsis("hello world", 8), "hello w…");
    }

    #[test]
    fn test_truncate_with_ellipsis_very_short() {
        assert_eq!(truncate_with_ellipsis("hello", 1), "…");
    }
}
