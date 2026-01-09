//! Input feature view.
//!
//! Pure rendering functions for the input area.
//! All input rendering logic is contained here.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::config::ThinkingLevel;
use crate::models::{ModelOption, model_supports_reasoning};
use crate::modes::tui::app::TuiState;
use crate::modes::tui::auth::AuthStatus;
use crate::modes::tui::thread::ThreadUsage;

/// Minimum height of the input area (lines, including borders).
const INPUT_HEIGHT_MIN: u16 = 5;

/// Maximum height of the input area as a percentage of screen height.
const INPUT_HEIGHT_MAX_PERCENT: f32 = 0.4;

/// Result of wrapping textarea content with Unicode-aware cursor tracking.
struct WrappedTextarea {
    /// Wrapped lines ready to render.
    lines: Vec<Line<'static>>,
    /// Visual row where cursor is (0-indexed, after wrapping).
    cursor_row: usize,
    /// Visual column where cursor is (display width units).
    cursor_col: usize,
}

/// Wraps textarea content respecting Unicode display width.
///
/// Handles multi-width characters (CJK, emoji) correctly by using
/// display width instead of character count for line breaking.
fn wrap_textarea(textarea: &tui_textarea::TextArea, available_width: usize) -> WrappedTextarea {
    let (cursor_line, cursor_col) = textarea.cursor();
    let cursor_line = cursor_line.min(textarea.lines().len().saturating_sub(1));

    let mut wrapped_lines: Vec<Line> = Vec::new();
    let mut visual_row = 0usize;
    let mut cursor_visual_row = 0usize;
    let mut cursor_visual_col = 0usize;

    for (line_idx, logical_line) in textarea.lines().iter().enumerate() {
        let is_cursor_line = line_idx == cursor_line;
        let line_visual_start = visual_row;

        if logical_line.is_empty() {
            // Empty line still takes one visual row
            wrapped_lines.push(Line::from(""));
            if is_cursor_line {
                cursor_visual_row = visual_row;
                cursor_visual_col = 0;
            }
            visual_row += 1;
            continue;
        }

        // Wrap the line at exact character width boundaries
        let mut current_width = 0usize;
        let mut line_start_byte = 0usize;

        for (byte_idx, ch) in logical_line.char_indices() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);

            if current_width + ch_width > available_width && current_width > 0 {
                // Wrap here - emit the current segment
                wrapped_lines.push(Line::from(Span::raw(
                    logical_line[line_start_byte..byte_idx].to_string(),
                )));
                line_start_byte = byte_idx;
                current_width = ch_width;
            } else {
                current_width += ch_width;
            }
        }

        // Emit remaining text
        wrapped_lines.push(Line::from(Span::raw(
            logical_line[line_start_byte..].to_string(),
        )));

        // Calculate cursor position if on this line
        if is_cursor_line {
            // Calculate display width up to cursor position
            let mut width_to_cursor = 0usize;
            for ch in logical_line.chars().take(cursor_col) {
                width_to_cursor += UnicodeWidthChar::width(ch).unwrap_or(0);
            }

            // Find which wrapped row and column
            let row_offset = if available_width > 0 {
                width_to_cursor / available_width
            } else {
                0
            };
            let col_offset = if available_width > 0 {
                width_to_cursor % available_width
            } else {
                0
            };
            cursor_visual_row = line_visual_start + row_offset;
            cursor_visual_col = col_offset;
        }

        // Count how many visual rows this logical line took
        let line_width = UnicodeWidthStr::width(logical_line.as_str());
        let wrapped_count = if available_width == 0 {
            1
        } else {
            line_width.div_ceil(available_width).max(1)
        };
        visual_row = line_visual_start + wrapped_count;
    }

    WrappedTextarea {
        lines: wrapped_lines,
        cursor_row: cursor_visual_row,
        cursor_col: cursor_visual_col,
    }
}

/// Calculates the dynamic input height based on content and terminal size.
///
/// - Minimum: INPUT_HEIGHT_MIN (5 lines with borders)
/// - Maximum: 40% of terminal height
/// - Expands when content has more than 3 lines
pub fn calculate_input_height(state: &TuiState, terminal_height: u16) -> u16 {
    let line_count = state.input.textarea.lines().len() as u16;

    // If 3 lines or fewer, use minimum height
    if line_count <= 3 {
        return INPUT_HEIGHT_MIN;
    }

    // Calculate max height (40% of screen)
    let max_height = ((terminal_height as f32) * INPUT_HEIGHT_MAX_PERCENT) as u16;

    // Add 2 for borders (top and bottom)
    let desired_height = line_count + 2;

    // Clamp between min and max
    desired_height.max(INPUT_HEIGHT_MIN).min(max_height)
}

/// Renders the input area with model info on top border and path on bottom border.
pub fn render_input(state: &TuiState, frame: &mut ratatui::Frame, area: Rect) {
    // Check if in handoff mode (any active handoff state)
    if state.input.handoff.is_active() {
        render_handoff_input(state, frame, area);
        return;
    }

    // Build top-left title: model name + auth type + thinking level
    let auth_indicator = match state.auth.auth_type {
        AuthStatus::OAuth => " (oauth)",
        AuthStatus::ApiKey => " (api-key)",
        AuthStatus::None => "",
    };

    // Build title spans: model + auth in normal style, thinking in dim style
    let base_style = Style::default().fg(Color::DarkGray);
    let thinking_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);

    let mut title_spans = vec![Span::styled(
        format!(" {}{}", state.config.model, auth_indicator),
        base_style,
    )];

    // Add thinking indicator with dim style (only when enabled + supported)
    if state.config.thinking_level != ThinkingLevel::Off
        && model_supports_reasoning(&state.config.model)
    {
        title_spans.push(Span::styled(
            format!(" [{}]", state.config.thinking_level.display_name()),
            thinking_style,
        ));
    }

    title_spans.push(Span::styled(" ", base_style));

    // Build top-right title: AMP-style usage display
    // Format: "{percentage}% of {context} · ${cost} (cached: ${savings})"
    let usage_spans = build_usage_display(&state.thread.usage, &state.config.model);

    // Build bottom-left title: detailed token breakdown
    let token_spans = build_token_breakdown(&state.thread.usage);

    // Build bottom-right title: path and git branch
    let bottom_title = if let Some(ref branch) = state.git_branch {
        format!(" {} ({}) ", state.display_path, branch)
    } else {
        format!(" {} ", state.display_path)
    };

    // Create a custom textarea rendering with our border titles
    // We need to render the textarea content inside our custom block
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(title_spans))
        .title_top(Line::from(usage_spans).alignment(Alignment::Right))
        .title_bottom(Line::from(token_spans).alignment(Alignment::Left))
        .title_bottom(
            Line::from(Span::styled(
                bottom_title,
                Style::default().fg(Color::DarkGray),
            ))
            .alignment(Alignment::Right),
        );

    let inner_area = block.inner(area);
    let available_width = inner_area.width as usize;

    if inner_area.width == 0 || inner_area.height == 0 {
        frame.render_widget(block, area);
        return;
    }

    // Wrap textarea content with Unicode-aware width calculation
    let wrapped = wrap_textarea(&state.input.textarea, available_width);

    // Calculate vertical scroll offset to keep cursor visible
    let total_visual_rows = wrapped.lines.len();
    let viewport_height = inner_area.height as usize;

    let scroll_offset = if total_visual_rows <= viewport_height {
        // All content fits, no scrolling needed
        0
    } else {
        // Content doesn't fit, calculate scroll to show cursor
        // Keep cursor in the middle third of the viewport when possible
        let ideal_cursor_position = viewport_height / 2;

        if wrapped.cursor_row < ideal_cursor_position {
            // Near top, show from beginning
            0
        } else if wrapped.cursor_row >= total_visual_rows.saturating_sub(ideal_cursor_position) {
            // Near bottom, show the last viewport_height lines
            total_visual_rows.saturating_sub(viewport_height)
        } else {
            // In middle, center the cursor
            wrapped.cursor_row.saturating_sub(ideal_cursor_position)
        }
    };

    // Slice visible lines based on scroll offset
    let visible_lines: Vec<Line> = wrapped
        .lines
        .into_iter()
        .skip(scroll_offset)
        .take(viewport_height)
        .collect();

    let input_paragraph = Paragraph::new(visible_lines).block(block);
    frame.render_widget(input_paragraph, area);

    // Adjust cursor position by scroll offset
    let cursor_x = inner_area.x + wrapped.cursor_col as u16;
    let cursor_y = inner_area.y + (wrapped.cursor_row.saturating_sub(scroll_offset) as u16);

    if cursor_x < inner_area.x + inner_area.width && cursor_y < inner_area.y + inner_area.height {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// Renders the input area in handoff mode with special styling.
fn render_handoff_input(state: &TuiState, frame: &mut ratatui::Frame, area: Rect) {
    // Handoff mode title - varies based on state
    let (title, border_color) = if state.input.handoff.is_generating() {
        (" handoff (generating prompt...) ", Color::Cyan)
    } else if state.input.handoff.is_ready() {
        // Generated prompt is ready for review
        (
            " handoff (review and Enter to start, Esc to cancel) ",
            Color::Green,
        )
    } else {
        // Waiting for goal input (Pending)
        (
            " handoff (enter goal for new thread, Esc to cancel) ",
            Color::Yellow,
        )
    };
    let title_style = Style::default().fg(border_color);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(title, title_style));

    let inner_area = block.inner(area);

    if inner_area.width == 0 || inner_area.height == 0 {
        frame.render_widget(block, area);
        return;
    }

    frame.render_widget(block, area);

    // Use shared wrapping helper for Unicode-aware width calculation
    let available_width = inner_area.width as usize;
    let wrapped = wrap_textarea(&state.input.textarea, available_width);

    // Viewport scrolling
    let visible_lines = inner_area.height as usize;
    let scroll_offset = if wrapped.cursor_row >= visible_lines {
        wrapped.cursor_row - visible_lines + 1
    } else {
        0
    };

    // Render lines
    for (i, line) in wrapped
        .lines
        .iter()
        .skip(scroll_offset)
        .take(visible_lines)
        .enumerate()
    {
        let y = inner_area.y + i as u16;
        // Clone spans and apply white foreground
        let styled_line = Line::from(
            line.spans
                .iter()
                .map(|s| Span::styled(s.content.clone(), Style::default().fg(Color::White)))
                .collect::<Vec<_>>(),
        );
        frame.render_widget(
            Paragraph::new(styled_line),
            Rect::new(inner_area.x, y, inner_area.width, 1),
        );
    }

    // Show cursor
    let cursor_y = inner_area.y + (wrapped.cursor_row.saturating_sub(scroll_offset)) as u16;
    let cursor_x = inner_area.x + wrapped.cursor_col as u16;
    if cursor_y < inner_area.y + inner_area.height && cursor_x < inner_area.x + inner_area.width {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// Builds the AMP-style usage display spans.
///
/// Format: "{percentage}% of {context} · ${cost} (cached)"
/// Example: "11% of 200k · $0.008 (cached)"
fn build_usage_display(usage: &ThreadUsage, model_id: &str) -> Vec<Span<'static>> {
    let usage_style = Style::default().fg(Color::DarkGray);
    let percentage_style = Style::default().fg(Color::Cyan);
    let cost_style = Style::default().fg(Color::Green);
    let cached_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::DIM);

    // Try to find the model to get pricing and context limit
    let model = ModelOption::find_by_id(model_id);

    match model {
        Some(m) => {
            let percentage = usage.context_percentage(m.context_limit);
            let cost = usage.calculate_cost(&m.pricing);
            let savings = usage.cache_savings(&m.pricing);

            let mut spans = vec![
                Span::styled(format!("{:.0}%", percentage), percentage_style),
                Span::styled(
                    format!(
                        " of {} · ",
                        ThreadUsage::format_context_limit(m.context_limit)
                    ),
                    usage_style,
                ),
                Span::styled(ThreadUsage::format_cost(cost), cost_style),
            ];

            // Show cache savings indicator if there are cache hits
            if savings > 0.001 {
                spans.push(Span::styled(
                    format!(" (saved {})", ThreadUsage::format_cost(savings)),
                    cached_style,
                ));
            } else if usage.cache_read_tokens > 0 {
                spans.push(Span::styled(" (cached)", cached_style));
            }

            spans.push(Span::styled(" ", usage_style));
            spans
        }
        None => {
            // Fallback: show raw token counts if model not found
            let total = usage.total_tokens();
            vec![
                Span::styled(ThreadUsage::format_tokens(total), usage_style),
                Span::styled(" tokens ", usage_style),
            ]
        }
    }
}

/// Builds the detailed token breakdown for bottom-left display.
///
/// Format: "↑{input} ↓{output} R{cache_read} W{cache_write}"
fn build_token_breakdown(usage: &ThreadUsage) -> Vec<Span<'static>> {
    let label_style = Style::default().fg(Color::DarkGray);
    let input_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM);
    let output_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::DIM);
    let cache_read_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::DIM);
    let cache_write_style = Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::DIM);

    vec![
        Span::styled(" ↑", input_style),
        Span::styled(ThreadUsage::format_tokens(usage.input_tokens), label_style),
        Span::styled(" ↓", output_style),
        Span::styled(ThreadUsage::format_tokens(usage.output_tokens), label_style),
        Span::styled(" R", cache_read_style),
        Span::styled(
            ThreadUsage::format_tokens(usage.cache_read_tokens),
            label_style,
        ),
        Span::styled(" W", cache_write_style),
        Span::styled(
            format!("{} ", ThreadUsage::format_tokens(usage.cache_write_tokens)),
            label_style,
        ),
    ]
}
