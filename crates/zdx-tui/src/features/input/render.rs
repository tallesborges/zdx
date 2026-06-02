//! Input feature view.
//!
//! Pure rendering functions for the input area.
//! All input rendering logic is contained here.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use zdx_engine::config::ThinkingLevel;
use zdx_engine::models::{ModelOption, model_supports_reasoning};
use zdx_engine::providers::{ProviderAuthMode, ProviderKind, provider_for_model};

use crate::common::{ratatui_text, ratatui_width};
use crate::input::TextBuffer;
use crate::state::{TuiState, fast_mode_enabled_for_model};
use crate::thread::ThreadUsage;

/// Minimum height of the input area (lines, including borders).
const INPUT_HEIGHT_MIN: u16 = 5;

/// Maximum height of the input area as a percentage of screen height.
const INPUT_HEIGHT_MAX_PERCENT: f32 = 0.4;

/// Style for placeholder text (bold magenta underlined to match transcript image placeholders).
fn placeholder_style() -> Style {
    Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

/// Result of wrapping textarea content with Unicode-aware cursor tracking.
struct WrappedTextarea {
    /// Wrapped lines ready to render.
    lines: Vec<Line<'static>>,
    /// Visual row where cursor is (0-indexed, after wrapping).
    cursor_row: usize,
    /// Visual column where cursor is (display width units).
    cursor_col: usize,
}

/// A segment of text that is either normal text or a placeholder.
#[derive(Debug)]
enum TextSegment<'a> {
    Normal(&'a str),
    Placeholder(&'a str),
}

/// Splits a line into segments of normal text and placeholders.
///
/// Placeholders are treated as atomic units that shouldn't be broken during wrapping.
fn split_into_segments<'a>(line: &'a str, placeholders: &[String]) -> Vec<TextSegment<'a>> {
    if placeholders.is_empty() {
        return vec![TextSegment::Normal(line)];
    }

    // Find all placeholder occurrences with their byte ranges
    let mut matches: Vec<(usize, usize)> = Vec::new();
    for placeholder in placeholders {
        let mut search_start = 0;
        while let Some(pos) = line[search_start..].find(placeholder.as_str()) {
            let abs_start = search_start + pos;
            let abs_end = abs_start + placeholder.len();
            matches.push((abs_start, abs_end));
            search_start = abs_start + 1;
        }
    }

    if matches.is_empty() {
        return vec![TextSegment::Normal(line)];
    }

    // Sort by start position and remove overlapping matches (keep first)
    matches.sort_by_key(|(start, _)| *start);
    let mut filtered: Vec<(usize, usize)> = Vec::new();
    for (start, end) in matches {
        if filtered
            .last()
            .is_none_or(|(_, prev_end)| start >= *prev_end)
        {
            filtered.push((start, end));
        }
    }

    // Build segments
    let mut segments = Vec::new();
    let mut cursor = 0;

    for (start, end) in filtered {
        if cursor < start {
            segments.push(TextSegment::Normal(&line[cursor..start]));
        }
        segments.push(TextSegment::Placeholder(&line[start..end]));
        cursor = end;
    }

    if cursor < line.len() {
        segments.push(TextSegment::Normal(&line[cursor..]));
    }

    segments
}

/// Wraps textarea content respecting Unicode display width.
///
/// Handles multi-width characters (CJK, emoji) correctly by using
/// display width instead of character count for line breaking.
/// Placeholders are treated as atomic units that won't be broken mid-text.
#[expect(
    clippy::too_many_lines,
    reason = "keeps placeholder wrapping and cursor mapping in one linear pass"
)]
fn wrap_textarea(
    textarea: &TextBuffer,
    available_width: usize,
    placeholders: &[String],
) -> WrappedTextarea {
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

        let segments = split_into_segments(logical_line, placeholders);
        let mut current_spans: Vec<Span<'static>> = Vec::new();
        let mut current_width = 0usize;
        let mut byte_pos = 0usize;
        let mut cursor_byte_pos = 0usize;
        if is_cursor_line {
            cursor_byte_pos = logical_line
                .char_indices()
                .nth(cursor_col)
                .map_or(logical_line.len(), |(i, _)| i);
        }

        for segment in segments {
            match segment {
                TextSegment::Placeholder(text) => {
                    let placeholder_width = ratatui_width(text);

                    // If placeholder doesn't fit on current line (and line has content), wrap first
                    if current_width > 0 && current_width + placeholder_width > available_width {
                        // Emit current line
                        wrapped_lines.push(Line::from(std::mem::take(&mut current_spans)));
                        visual_row += 1;
                        current_width = 0;
                    }

                    // Check cursor position before adding placeholder
                    if is_cursor_line
                        && byte_pos <= cursor_byte_pos
                        && cursor_byte_pos < byte_pos + text.len()
                    {
                        // Cursor is inside this placeholder - place it at the start
                        cursor_visual_row = visual_row;
                        cursor_visual_col = current_width;
                    }

                    current_spans.push(Span::styled(
                        ratatui_text(text).into_owned(),
                        placeholder_style(),
                    ));
                    current_width += placeholder_width;
                    byte_pos += text.len();

                    // Check if cursor is right after the placeholder
                    if is_cursor_line && cursor_byte_pos == byte_pos {
                        cursor_visual_row = visual_row;
                        cursor_visual_col = current_width;
                    }
                }
                TextSegment::Normal(text) => {
                    let mut segment_start = 0;

                    for (grapheme_offset, grapheme) in text.grapheme_indices(true) {
                        let grapheme_width = ratatui_width(grapheme);
                        let grapheme_end = grapheme_offset + grapheme.len();

                        if current_width + grapheme_width > available_width && current_width > 0 {
                            // Emit accumulated normal text first
                            if segment_start < grapheme_offset {
                                current_spans.push(Span::raw(
                                    ratatui_text(&text[segment_start..grapheme_offset])
                                        .into_owned(),
                                ));
                            }
                            // Emit current line
                            wrapped_lines.push(Line::from(std::mem::take(&mut current_spans)));
                            visual_row += 1;
                            current_width = 0;
                            segment_start = grapheme_offset;
                        }

                        // Track cursor position
                        if is_cursor_line
                            && byte_pos + grapheme_offset <= cursor_byte_pos
                            && cursor_byte_pos < byte_pos + grapheme_end
                        {
                            cursor_visual_row = visual_row;
                            cursor_visual_col = current_width;
                        }

                        current_width += grapheme_width;

                        if is_cursor_line && byte_pos + grapheme_end == cursor_byte_pos {
                            cursor_visual_row = visual_row;
                            cursor_visual_col = current_width;
                        }
                    }

                    // Add remaining text from this segment
                    if segment_start < text.len() {
                        current_spans
                            .push(Span::raw(ratatui_text(&text[segment_start..]).into_owned()));
                    }

                    // Check cursor at end of segment
                    if is_cursor_line && byte_pos + text.len() == cursor_byte_pos {
                        cursor_visual_row = visual_row;
                        cursor_visual_col = current_width;
                    }

                    byte_pos += text.len();
                }
            }
        }

        // Emit final line for this logical line
        if !current_spans.is_empty() || wrapped_lines.len() == line_visual_start {
            wrapped_lines.push(Line::from(current_spans));
            visual_row += 1;
        }

        // Handle cursor at very end of line
        if is_cursor_line && cursor_byte_pos >= logical_line.len() {
            cursor_visual_row = visual_row.saturating_sub(1);
            cursor_visual_col = current_width;
        }
    }

    WrappedTextarea {
        lines: wrapped_lines,
        cursor_row: cursor_visual_row,
        cursor_col: cursor_visual_col,
    }
}

/// Calculates the dynamic input height based on content and terminal size.
///
/// - Minimum: `INPUT_HEIGHT_MIN` (5 lines with borders)
/// - Maximum: 40% of terminal height
/// - Expands when content has more than 3 lines
pub fn calculate_input_height(state: &TuiState, terminal_height: u16) -> u16 {
    let line_count = state.input.textarea.lines().len() as u16;

    // If 3 lines or fewer, use minimum height
    if line_count <= 3 {
        return INPUT_HEIGHT_MIN;
    }

    // Calculate max height (40% of screen)
    let max_height = (f32::from(terminal_height) * INPUT_HEIGHT_MAX_PERCENT) as u16;

    // Add 2 for borders (top and bottom)
    let desired_height = line_count + 2;

    // Clamp between min and max
    desired_height.max(INPUT_HEIGHT_MIN).min(max_height)
}

/// Renders the input area with model info on top border and path on bottom border.
pub fn render_input(state: &TuiState, frame: &mut ratatui::Frame, area: Rect) {
    render_input_with_cursor(state, frame, area, true);
}

/// Renders the input area. When `show_cursor` is false, the terminal cursor is not placed.
pub fn render_input_with_cursor(
    state: &TuiState,
    frame: &mut ratatui::Frame,
    area: Rect,
    show_cursor: bool,
) {
    // Modal sub-features (handoff, prompt-builder) own the
    // composer while active. Dispatch their dedicated renderer instead of
    // drawing the normal title chrome.
    if try_render_modal_input(state, frame, area, show_cursor) {
        return;
    }

    // Build top-left title: model name + fast/thinking badges
    let base_style = Style::default().fg(Color::DarkGray);
    let fast_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM);
    let thinking_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);

    let mut title_spans = vec![Span::styled(format!(" {}", state.config.model), base_style)];

    if fast_mode_enabled_for_model(&state.config, &state.config.model) {
        title_spans.push(Span::styled(" [F]", fast_style));
    }

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
    let provider = provider_for_model(&state.config.model);
    let usage_spans = build_usage_display(&state.thread.usage, &state.config.model, provider);

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

    // Extract placeholder strings for visual highlighting (pastes + images)
    let mut placeholders: Vec<String> = state
        .input
        .pending_pastes
        .iter()
        .map(|p| p.placeholder.clone())
        .collect();
    placeholders.extend(
        state
            .input
            .pending_images
            .iter()
            .map(|img| img.placeholder.clone()),
    );

    // Wrap textarea content with Unicode-aware width calculation
    let wrapped = wrap_textarea(&state.input.textarea, available_width, &placeholders);

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

    if show_cursor
        && cursor_x < inner_area.x + inner_area.width
        && cursor_y < inner_area.y + inner_area.height
    {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// Dispatches to the matching modal renderer when a sub-feature owns the
/// composer. Returns `true` if a modal renderer was invoked so the caller
/// can skip its own (normal-mode) drawing.
fn try_render_modal_input(
    state: &TuiState,
    frame: &mut ratatui::Frame,
    area: Rect,
    show_cursor: bool,
) -> bool {
    if state.input.handoff.is_active() {
        render_handoff_input(state, frame, area, show_cursor);
        return true;
    }
    if state.input.prompt_builder.is_active() {
        render_prompt_builder_input(state, frame, area, show_cursor);
        return true;
    }
    false
}

/// Renders the input area in handoff mode with special styling.
fn render_handoff_input(
    state: &TuiState,
    frame: &mut ratatui::Frame,
    area: Rect,
    show_cursor: bool,
) {
    // Handoff mode title - varies based on state
    let (title, border_color) = if state.input.handoff.is_generating() {
        (" handoff (generating prompt...) ", Color::Cyan)
    } else if state.input.handoff.is_ready() {
        // Generated prompt is ready for review
        (
            " handoff (review and Enter to open in new tab, Esc to cancel) ",
            Color::Green,
        )
    } else {
        // Waiting for next-message input (Pending)
        (
            " handoff (type your first message for the new chat, Esc to cancel) ",
            Color::Yellow,
        )
    };
    render_status_input(state, frame, area, show_cursor, title, border_color);
}

/// Renders the input area in prompt-builder mode with special styling.
fn render_prompt_builder_input(
    state: &TuiState,
    frame: &mut ratatui::Frame,
    area: Rect,
    show_cursor: bool,
) {
    let (title, border_color) = if state.input.prompt_builder.is_generating() {
        (" prompt-builder (generating prompt...) ", Color::Cyan)
    } else if state.input.prompt_builder.is_ready() {
        // Generated prompt is awaiting review.
        (
            " prompt-builder (review — Enter to send, type to edit, Esc to undo) ",
            Color::Green,
        )
    } else {
        // Pending: waiting for the user's intent.
        (
            " prompt-builder (describe your intent, Esc to cancel) ",
            Color::Yellow,
        )
    };
    render_status_input(state, frame, area, show_cursor, title, border_color);
}

/// Shared single-state composer renderer used by handoff and prompt-builder
/// modes. Draws the textarea inside a single-color border with the given
/// title, scrolling so the cursor stays visible. Inner text is rendered
/// uniformly white — paste/image placeholder styling is intentionally not
/// preserved here because these modes are short-lived modal flows where the
/// border/title carry the visual signal.
fn render_status_input(
    state: &TuiState,
    frame: &mut ratatui::Frame,
    area: Rect,
    show_cursor: bool,
    title: &str,
    border_color: Color,
) {
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

    // Pastes/images are rare in modal flows — skip the per-render Vec
    // allocation in the common case where neither is attached.
    let placeholders: Vec<String> =
        if state.input.pending_pastes.is_empty() && state.input.pending_images.is_empty() {
            Vec::new()
        } else {
            state.input.all_placeholder_strings()
        };

    // Use common wrapping helper for Unicode-aware width calculation
    let available_width = inner_area.width as usize;
    let wrapped = wrap_textarea(&state.input.textarea, available_width, &placeholders);

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
    if show_cursor
        && cursor_y < inner_area.y + inner_area.height
        && cursor_x < inner_area.x + inner_area.width
    {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// Builds the AMP-style usage display spans.
///
/// Format: "{percentage}% of {context} · ${cost} (cached)"
/// Example: "11% of 200k · $0.008 (cached)"
fn build_usage_display(
    usage: &ThreadUsage,
    model_id: &str,
    provider: ProviderKind,
) -> Vec<Span<'static>> {
    let usage_style = Style::default().fg(Color::DarkGray);
    let percentage_style = Style::default().fg(Color::Cyan);
    let cost_style = Style::default().fg(Color::Green);
    let cached_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::DIM);

    let show_pricing =
        provider.auth_mode() == ProviderAuthMode::ApiKey && !provider.is_subscription();
    let show_subscription = provider.is_subscription();

    // Try to find the model to get pricing and context limit
    let model = ModelOption::find_by_id(model_id);

    if let Some(m) = model {
        let percentage = usage.context_percentage(m.context_limit);

        let mut spans = vec![
            Span::styled(format!("{percentage:.0}%"), percentage_style),
            Span::styled(
                format!(" of {}", ThreadUsage::format_context_limit(m.context_limit)),
                usage_style,
            ),
        ];

        if show_subscription {
            spans.push(Span::styled(" · (subscription)", cached_style));
        }

        if show_pricing {
            let cost = usage.calculate_cost(&m.pricing);
            let savings = usage.cache_savings(&m.pricing);

            spans.push(Span::styled(" · ", usage_style));
            spans.push(Span::styled(ThreadUsage::format_cost(cost), cost_style));

            // Show cache savings indicator if there are cache hits
            if savings > 0.001 {
                spans.push(Span::styled(
                    format!(" (saved {})", ThreadUsage::format_cost(savings)),
                    cached_style,
                ));
            } else if usage.cache_read_tokens > 0 {
                spans.push(Span::styled(" (cached)", cached_style));
            }
        }

        spans.push(Span::styled(" ", usage_style));
        spans
    } else {
        // Fallback: show raw token counts if model not found
        let total = usage.total_tokens();
        vec![
            Span::styled(ThreadUsage::format_tokens(total), usage_style),
            Span::styled(" tokens ", usage_style),
        ]
    }
}

/// Builds the detailed token breakdown for bottom-left display.
///
/// Format: "↑{input} ↓{output} `R{cache_read`} `W{cache_write`}"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::CursorMove;

    #[test]
    fn wrap_textarea_preserves_emoji_graphemes() {
        for text in ["⚠️⚠️", "👩‍🚀👩‍🚀", "👍🏽👍🏽", "éé"] {
            for width in [1, 2, 3] {
                let wrapped = wrap_textarea_with_text(text, width);
                let lines = wrapped_line_text(&wrapped);
                assert_wrapped_line_invariants(text, &lines, width);
            }
        }
    }

    #[test]
    fn wrap_textarea_maps_cursor_around_emoji_graphemes() {
        let mut textarea = TextBuffer::default();
        textarea.insert_str("a👩‍🚀b");
        textarea.move_cursor(CursorMove::Head);
        textarea.move_cursor(CursorMove::Forward);

        let before_emoji = wrap_textarea(&textarea, 20, &[]);
        assert_eq!((before_emoji.cursor_row, before_emoji.cursor_col), (0, 1));

        textarea.move_cursor(CursorMove::Forward);
        let inside_emoji = wrap_textarea(&textarea, 20, &[]);
        assert_eq!((inside_emoji.cursor_row, inside_emoji.cursor_col), (0, 1));

        textarea.move_cursor(CursorMove::Forward);
        textarea.move_cursor(CursorMove::Forward);
        let after_emoji = wrap_textarea(&textarea, 20, &[]);
        assert_eq!((after_emoji.cursor_row, after_emoji.cursor_col), (0, 3));
    }

    fn wrap_textarea_with_text(text: &str, width: usize) -> WrappedTextarea {
        let mut textarea = TextBuffer::default();
        textarea.insert_str(text);
        wrap_textarea(&textarea, width, &[])
    }

    fn wrapped_line_text(wrapped: &WrappedTextarea) -> Vec<String> {
        wrapped
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    fn assert_wrapped_line_invariants(original: &str, lines: &[String], width: usize) {
        let joined: String = lines.iter().map(String::as_str).collect();
        assert_eq!(joined, ratatui_text(original));

        for line in lines {
            let line_width = ratatui_width(line);
            let grapheme_count = line.graphemes(true).count();
            let is_single_wide_grapheme = grapheme_count == 1 && line_width > width;

            assert!(
                line_width <= width || is_single_wide_grapheme,
                "{line:?} width {line_width} exceeds {width}"
            );
        }
    }
}
