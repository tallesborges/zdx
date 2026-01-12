//! Thread feature view.
//!
//! Rendering functions for the thread picker overlay.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use zdx_core::core::thread_log::{self, short_thread_id};

use crate::modes::tui::overlays::{ThreadPickerState, ThreadScope};

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

    let visible_threads = picker.visible_threads();
    let visible_count = visible_threads.len().min(MAX_VISIBLE_THREADS);
    let thread_count = match picker.scope {
        ThreadScope::All => visible_threads.len(),
        ThreadScope::Current => picker.all_threads.len(),
    };

    let picker_width = 60;
    let picker_height = (visible_count as u16 + 5).max(7);

    let picker_area = calculate_overlay_area(area, input_top_y, picker_width, picker_height);
    let title = match picker.scope {
        ThreadScope::All => format!("Threads ({})", thread_count),
        ThreadScope::Current => format!("Threads ({}/{})", visible_threads.len(), thread_count),
    };
    render_overlay_container(frame, picker_area, &title, Color::Blue);

    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    if visible_threads.is_empty() {
        let empty_msg = Paragraph::new(match picker.scope {
            ThreadScope::Current => "No threads in this workspace",
            ThreadScope::All => "No threads found",
        })
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

    let items: Vec<ListItem> = visible_threads
        .iter()
        .skip(picker.offset)
        .take(list_height)
        .map(|thread| {
            let thread = *thread;
            let timestamp = thread
                .modified
                .and_then(thread_log::format_timestamp_relative)
                .unwrap_or_else(|| "—".to_string());

            // Show title if available, otherwise fall back to short ID
            let display_name = if thread.title.is_some() {
                thread.display_title()
            } else {
                short_thread_id(&thread.id).to_string()
            };

            // Account for highlight symbol "▶ " (3 chars wide)
            let highlight_width = 3;
            let available_width = (inner_area.width as usize).saturating_sub(highlight_width);
            let date_width = timestamp.width();
            // Reserve space for date + gap (minimum 1 space)
            let name_max_width = available_width.saturating_sub(date_width + 2);
            let display_name = truncate_with_ellipsis(&display_name, name_max_width);
            let gap = available_width
                .saturating_sub(display_name.width() + date_width)
                .max(1);

            let line = Line::from(vec![
                Span::styled(display_name, Style::default().fg(Color::White)),
                Span::styled(" ".repeat(gap), Style::default()),
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
            InputHint::new(
                "Ctrl+T",
                match picker.scope {
                    ThreadScope::Current => "all",
                    ThreadScope::All => "current",
                },
            ),
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
