//! Thread feature view.
//!
//! Rendering functions for the thread picker overlay.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;
use zdx_core::core::thread_persistence::{self, short_thread_id};

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
        calculate_overlay_area, render_overlay_container, render_separator,
    };

    let tree_items = picker.visible_tree_items();
    let visible_count = tree_items.len().min(MAX_VISIBLE_THREADS);
    let thread_count = match picker.scope {
        ThreadScope::All => picker.all_threads.len(),
        ThreadScope::Current => picker
            .all_threads
            .iter()
            .filter(|t| t.root_path.as_deref() == Some(picker.current_root.as_str()))
            .count(),
    };

    let picker_width = 60;
    // Add 2 rows for filter input + separator
    let picker_height = (visible_count as u16 + 7).max(9);

    let picker_area = calculate_overlay_area(area, input_top_y, picker_width, picker_height);
    let title = thread_picker_title(picker.scope, tree_items.len(), thread_count);
    render_overlay_container(frame, picker_area, &title, Color::Magenta);

    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    render_filter_input(frame, picker, inner_area);

    render_separator(frame, inner_area, 1);

    if tree_items.is_empty() {
        render_empty_picker(frame, picker, inner_area);
        return;
    }

    // Account for filter (1) + separator (1) + hints (2)
    let list_height = inner_area.height.saturating_sub(4) as usize;

    let list_area = Rect::new(
        inner_area.x,
        inner_area.y + 2,
        inner_area.width,
        list_height as u16,
    );

    let items: Vec<ListItem> = tree_items
        .iter()
        .skip(picker.offset)
        .take(list_height)
        .map(|item| build_thread_list_item(item, picker, inner_area.width))
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::Magenta)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    let visible_selected = picker.selected.saturating_sub(picker.offset);
    list_state.select(Some(visible_selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, inner_area, 2 + list_height as u16);

    render_picker_hints(frame, picker, inner_area);
}

fn thread_picker_title(scope: ThreadScope, visible_count: usize, total_count: usize) -> String {
    match scope {
        ThreadScope::All => format!("Threads ({total_count})"),
        ThreadScope::Current => format!("Threads ({visible_count}/{total_count})"),
    }
}

fn render_filter_input(frame: &mut Frame, picker: &ThreadPickerState, inner_area: Rect) {
    let max_filter_len = inner_area.width.saturating_sub(4) as usize;
    let filter_display = if picker.filter.len() > max_filter_len {
        let truncated = &picker.filter[picker.filter.len() - max_filter_len..];
        format!("…{truncated}")
    } else {
        picker.filter.clone()
    };
    let filter_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::styled(filter_display, Style::default().fg(Color::Magenta)),
        Span::styled("█", Style::default().fg(Color::Magenta)),
    ]);
    frame.render_widget(
        Paragraph::new(filter_line),
        Rect::new(inner_area.x, inner_area.y, inner_area.width, 1),
    );
}

fn render_empty_picker(frame: &mut Frame, picker: &ThreadPickerState, inner_area: Rect) {
    let message = if picker.filter.is_empty() {
        match picker.scope {
            ThreadScope::Current => "No threads in this workspace",
            ThreadScope::All => "No threads found",
        }
    } else {
        "No matching threads"
    };
    let empty_area = Rect::new(
        inner_area.x,
        inner_area.y + 2,
        inner_area.width,
        inner_area.height.saturating_sub(4),
    );
    frame.render_widget(
        Paragraph::new(message)
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center),
        empty_area,
    );
    let list_height = inner_area.height.saturating_sub(4);
    crate::overlays::render_utils::render_separator(frame, inner_area, 2 + list_height);
    render_picker_hints(frame, picker, inner_area);
}

fn build_thread_list_item(
    item: &crate::thread::ThreadDisplayItem<'_>,
    picker: &ThreadPickerState,
    inner_width: u16,
) -> ListItem<'static> {
    let thread = item.summary;
    let timestamp = thread
        .modified
        .and_then(thread_persistence::format_timestamp_relative)
        .unwrap_or_else(|| "—".to_string());
    let display_name = if thread.title.is_some() {
        thread.display_title()
    } else {
        short_thread_id(&thread.id).to_string()
    };

    let capped_depth = item.depth.min(4);
    let indent_spaces = if capped_depth > 1 {
        (capped_depth - 1) * 4
    } else {
        0
    };
    let tree_prefix = format!(
        "{}{}",
        " ".repeat(indent_spaces),
        if item.depth > 0 { "└── " } else { "" }
    );
    let handoff_label = if item.is_handoff { "[handoff] " } else { "" };
    let is_current = picker
        .current_thread_id
        .as_ref()
        .is_some_and(|id| id == &thread.id);
    let current_label = if is_current { "(current) " } else { "" };

    let highlight_width = 3;
    let available_width = (inner_width as usize).saturating_sub(highlight_width);
    let date_width = timestamp.width();
    let name_max_width = available_width.saturating_sub(
        tree_prefix.width() + handoff_label.width() + current_label.width() + date_width + 2,
    );
    let display_name = truncate_with_ellipsis(&display_name, name_max_width);
    let gap = available_width
        .saturating_sub(
            tree_prefix.width()
                + handoff_label.width()
                + display_name.width()
                + current_label.width()
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
}

fn render_picker_hints(frame: &mut Frame, picker: &ThreadPickerState, inner_area: Rect) {
    use crate::overlays::render_utils::{InputHint, render_hints};
    let copy_hint = if picker.should_show_copied() {
        InputHint::new("✓", "Copied!")
    } else {
        InputHint::new("y", "copy id")
    };
    let toggle_hint = match picker.scope {
        ThreadScope::Current => "all",
        ThreadScope::All => "current",
    };
    render_hints(
        frame,
        inner_area,
        &[
            InputHint::new("↑↓", "navigate"),
            InputHint::new("Enter", "select"),
            copy_hint,
            InputHint::new("Ctrl+T", toggle_hint),
            InputHint::new("Esc", "cancel"),
        ],
        Color::Magenta,
    );
}
