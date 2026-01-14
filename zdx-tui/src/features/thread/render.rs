//! Thread feature view.
//!
//! Rendering functions for the thread picker overlay.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;
use zdx_core::core::thread_log::{self, short_thread_id};

use crate::common::truncate_with_ellipsis;
use crate::overlays::{ThreadPickerState, ThreadScope};

/// Maximum number of threads visible in the picker list.
///
/// This constant is the source of truth for scroll calculations in both
/// rendering and update logic. The actual visible height is calculated
/// dynamically but capped at this value.
pub const MAX_VISIBLE_THREADS: usize = 10;

/// Renders the thread picker overlay.
pub fn render_thread_picker(
    frame: &mut Frame,
    picker: &ThreadPickerState,
    area: Rect,
    input_top_y: u16,
) {
    use crate::overlays::render_utils::{
        InputHint, calculate_overlay_area, render_hints, render_overlay_container, render_separator,
    };

    let tree_items = picker.visible_tree_items();
    let visible_count = tree_items.len().min(MAX_VISIBLE_THREADS);
    let thread_count = match picker.scope {
        ThreadScope::All => tree_items.len(),
        ThreadScope::Current => picker.all_threads.len(),
    };

    let picker_width = 60;
    let picker_height = (visible_count as u16 + 5).max(7);

    let picker_area = calculate_overlay_area(area, input_top_y, picker_width, picker_height);
    let title = match picker.scope {
        ThreadScope::All => format!("Threads ({})", thread_count),
        ThreadScope::Current => format!("Threads ({}/{})", tree_items.len(), thread_count),
    };
    render_overlay_container(frame, picker_area, &title, Color::Blue);

    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    if tree_items.is_empty() {
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

    let items: Vec<ListItem> = tree_items
        .iter()
        .skip(picker.offset)
        .take(list_height)
        .map(|item| {
            let thread = item.summary;
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

            // Calculate tree prefix for hierarchical display
            // For depth 1, branch starts at column 0 (aligned with parent)
            // For depth 2+, indent by (depth-1) * 4 to align under previous level's text
            let capped_depth = item.depth.min(4);
            let indent_spaces = if capped_depth > 1 {
                (capped_depth - 1) * 4
            } else {
                0
            };
            let indent_str: String = " ".repeat(indent_spaces);
            // Branch character for child threads (depth > 0)
            let branch_str = if item.depth > 0 { "└── " } else { "" };
            // Handoff label for threads created via handoff
            let handoff_label = if item.is_handoff { "[handoff] " } else { "" };
            // Current thread indicator (appears before thread name)
            let is_current = picker
                .current_thread_id
                .as_ref()
                .is_some_and(|id| id == &thread.id);
            let current_label = if is_current { "(current) " } else { "" };

            let tree_prefix = format!("{}{}", indent_str, branch_str);
            let tree_prefix_width = tree_prefix.width();
            let handoff_label_width = handoff_label.width();
            let current_label_width = current_label.width();

            // Account for highlight symbol "▶ " (3 chars wide)
            let highlight_width = 3;
            let available_width = (inner_area.width as usize).saturating_sub(highlight_width);
            let date_width = timestamp.width();
            // Reserve space for: tree_prefix + handoff_label + current_label + date + gap (minimum 1 space)
            let name_max_width = available_width.saturating_sub(
                tree_prefix_width + handoff_label_width + current_label_width + date_width + 2,
            );
            let display_name = truncate_with_ellipsis(&display_name, name_max_width);
            let gap = available_width
                .saturating_sub(
                    tree_prefix_width
                        + handoff_label_width
                        + display_name.width()
                        + current_label_width
                        + date_width,
                )
                .max(1);

            let line = Line::from(vec![
                Span::styled(tree_prefix, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    handoff_label.to_string(),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(current_label.to_string(), Style::default().fg(Color::Cyan)),
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
