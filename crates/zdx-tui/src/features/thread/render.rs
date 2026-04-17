//! Thread feature view.
//!
//! Rendering functions for the thread picker overlay.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;
use zdx_engine::core::thread_persistence::{self, short_thread_id};

use crate::common::truncate_with_ellipsis;
use crate::overlays::{ThreadPickerState, ThreadScope};

/// Maximum number of threads visible in the picker list.
///
/// This constant is the source of truth for scroll calculations in both
/// rendering and update logic. The actual visible height is calculated
/// dynamically but capped at this value.
pub const MAX_VISIBLE_THREADS: usize = 10;

/// Renders the thread picker overlay.
///
/// - `Switch` mode (opened from command palette): centered modal with title + hints.
/// - `Insert` mode (opened via `@@`): bottom-left inline dropdown, no title/hints.
pub fn render_thread_picker(
    frame: &mut Frame,
    picker: &ThreadPickerState,
    area: Rect,
    input_top_y: u16,
) {
    if picker.mode.is_switch() {
        render_thread_picker_modal(frame, picker, area, input_top_y);
    } else {
        render_thread_picker_inline(frame, picker, area, input_top_y);
    }
}

/// Centered modal style — used for Switch mode (command palette / Ctrl+T).
fn render_thread_picker_modal(
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
    let picker_height = (visible_count as u16 + 7).max(9);

    let picker_area = calculate_overlay_area(area, input_top_y, picker_width, picker_height);
    let title = thread_picker_title(picker.scope, tree_items.len(), thread_count);
    render_overlay_container(frame, picker_area, &title, Color::Magenta);

    let inner = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    render_filter_input(frame, picker, inner);
    render_separator(frame, inner, 1);

    if tree_items.is_empty() {
        render_empty_picker(frame, picker, inner, /* modal */ true);
        return;
    }

    // filter(1) + sep(1) top, sep(1) + hints(1) bottom
    let list_height = inner.height.saturating_sub(4) as usize;
    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, list_height as u16);

    let items: Vec<ListItem> = tree_items
        .iter()
        .skip(picker.offset)
        .take(list_height)
        .map(|item| build_thread_list_item(item, picker, inner.width))
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
    list_state.select(Some(picker.selected.saturating_sub(picker.offset)));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, inner, 2 + list_height as u16);
    render_picker_hints(frame, picker, inner);
}

/// Inline dropdown style — used for Insert mode (`@@`).
fn render_thread_picker_inline(
    frame: &mut Frame,
    picker: &ThreadPickerState,
    area: Rect,
    input_top_y: u16,
) {
    use ratatui::widgets::{Block, Borders, Clear};

    use crate::overlays::render_utils::render_separator;

    let tree_items = picker.visible_tree_items();
    let visible_count = tree_items.len().min(MAX_VISIBLE_THREADS);

    // Width: wide, left-anchored, leaves a small right margin
    let picker_width = area.width.saturating_sub(4).min(80);

    // Height: 2 borders + filter row + separator + list rows (no title/hints overhead)
    let inner_height: u16 = if tree_items.is_empty() {
        3 // filter + sep + empty message
    } else {
        2 + visible_count as u16 // filter + sep + list
    };
    let picker_height = (inner_height + 2).max(5);

    // Position: bottom of available space, just above the input bar
    let popup_y = input_top_y.saturating_sub(picker_height);
    let popup = Rect::new(0, popup_y, picker_width.min(area.width), picker_height);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    render_filter_input(frame, picker, inner);
    render_separator(frame, inner, 1);

    if tree_items.is_empty() {
        render_empty_picker(frame, picker, inner, /* modal */ false);
        return;
    }

    // list starts after filter (1) + separator (1)
    let list_height = inner.height.saturating_sub(2) as usize;
    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, list_height as u16);

    let items: Vec<ListItem> = tree_items
        .iter()
        .skip(picker.offset)
        .take(list_height)
        .map(|item| build_thread_list_item(item, picker, inner.width))
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    list_state.select(Some(picker.selected.saturating_sub(picker.offset)));
    frame.render_stateful_widget(list, list_area, &mut list_state);
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

/// Renders the empty-state message.
///
/// `modal` controls how much vertical space the empty area takes:
/// - modal=true: subtracts hints + separator from bottom (4 rows overhead)
/// - modal=false: subtracts only filter + separator from top (2 rows overhead)
fn render_empty_picker(
    frame: &mut Frame,
    picker: &ThreadPickerState,
    inner_area: Rect,
    modal: bool,
) {
    let message = if picker.filter.is_empty() {
        match picker.scope {
            ThreadScope::Current => "No threads in this workspace",
            ThreadScope::All => "No threads found",
        }
    } else {
        "No matching threads"
    };
    let bottom_overhead: u16 = if modal { 2 } else { 0 };
    let empty_height = inner_area.height.saturating_sub(2 + bottom_overhead);
    let empty_area = Rect::new(
        inner_area.x,
        inner_area.y + 2,
        inner_area.width,
        empty_height,
    );
    frame.render_widget(
        Paragraph::new(message)
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center),
        empty_area,
    );
    if modal {
        use crate::overlays::render_utils::render_separator;
        render_separator(frame, inner_area, 2 + empty_height);
        render_picker_hints(frame, picker, inner_area);
    }
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
    let running_label = if picker.is_thread_active(&thread.id) {
        "[running] "
    } else {
        ""
    };
    let is_current = picker
        .current_thread_id
        .as_ref()
        .is_some_and(|id| id == &thread.id);
    let current_label = if is_current { "(current) " } else { "" };

    let highlight_width = 3;
    let available_width = (inner_width as usize).saturating_sub(highlight_width);
    let date_width = timestamp.width();
    let name_max_width = available_width.saturating_sub(
        tree_prefix.width()
            + handoff_label.width()
            + running_label.width()
            + current_label.width()
            + date_width
            + 2,
    );
    let display_name = truncate_with_ellipsis(&display_name, name_max_width);
    let gap = available_width
        .saturating_sub(
            tree_prefix.width()
                + handoff_label.width()
                + running_label.width()
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
        Span::styled(running_label.to_string(), Style::default().fg(Color::Green)),
        Span::styled(current_label.to_string(), Style::default().fg(Color::Cyan)),
        Span::styled(display_name, Style::default().fg(Color::White)),
        Span::styled(" ".repeat(gap), Style::default()),
        Span::styled(timestamp, Style::default().fg(Color::DarkGray)),
    ]);
    ListItem::new(line)
}
