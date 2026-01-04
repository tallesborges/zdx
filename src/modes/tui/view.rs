//! Pure view/render functions for the TUI.
//!
//! This module contains all rendering logic. Functions here:
//! - Take `&AppState` by immutable reference
//! - Draw to a ratatui Frame
//! - Never mutate state or return effects
//!
//! The separation from TuiRuntime eliminates borrow-checker conflicts
//! that previously required cloning state for rendering.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::modes::tui::input;
use crate::modes::tui::state::{AgentState, AppState, TuiState};
use crate::modes::tui::transcript::{LineMapping, SelectionState, Style as TranscriptStyle, StyledLine};

/// Height of status line below input.
const STATUS_HEIGHT: u16 = 1;

/// Horizontal margin for the transcript area (left and right).
/// Transcript horizontal margin (padding on each side).
pub const TRANSCRIPT_MARGIN: u16 = 1;

/// Spinner frames for status line animation.
const SPINNER_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

/// Spinner speed divisor (render frames per spinner frame).
pub const SPINNER_SPEED_DIVISOR: usize = 6;

/// Renders the entire TUI to the frame.
///
/// This is a pure render function - it only reads state and draws to frame.
/// No mutations, no side effects.
pub fn view(app: &AppState, frame: &mut Frame) {
    let area = frame.area();
    let state = &app.tui;

    // Calculate dynamic input height based on content
    let input_height = input::calculate_input_height(state, area.height);

    // Get terminal size for transcript rendering (account for margins)
    let transcript_width = area.width.saturating_sub(TRANSCRIPT_MARGIN * 2) as usize;

    // Calculate transcript pane height (no header now)
    let transcript_height = area.height.saturating_sub(input_height + STATUS_HEIGHT) as usize;

    // Pre-render transcript lines
    // is_lazy indicates whether lazy rendering was used (lines are already scrolled)
    let (all_lines, is_lazy) = render_transcript(state, transcript_width);
    let total_lines = if is_lazy {
        // For lazy rendering, use cached total line count for scroll calculations
        state.transcript.scroll.cached_line_count
    } else {
        all_lines.len()
    };

    // Get visible lines - handling differs based on rendering mode
    let content_lines: Vec<Line<'static>> = if is_lazy {
        // Lazy rendering already returned only visible lines, no slicing needed
        all_lines
    } else {
        // Full rendering: apply scroll offset to slice visible portion
        let scroll_offset = {
            let max_offset = total_lines.saturating_sub(transcript_height);
            if state.transcript.scroll.is_following() {
                total_lines.saturating_sub(transcript_height)
            } else {
                state
                    .transcript
                    .scroll
                    .get_offset(transcript_height)
                    .min(max_offset)
            }
        };

        let visible_end = (scroll_offset + transcript_height).min(total_lines);
        all_lines
            .into_iter()
            .skip(scroll_offset)
            .take(visible_end - scroll_offset)
            .collect()
    };

    // Bottom-align: add padding at top when content doesn't fill the screen
    let visible_lines: Vec<Line<'static>> = if content_lines.len() < transcript_height {
        let padding_count = transcript_height - content_lines.len();
        let mut padded = vec![Line::default(); padding_count];
        padded.extend(content_lines);
        padded
    } else {
        content_lines
    };

    // Create layout: transcript, input, status
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),                // Transcript
            Constraint::Length(input_height),  // Input (dynamic)
            Constraint::Length(STATUS_HEIGHT), // Status line
        ])
        .split(area);

    // Transcript area with horizontal margins
    // NOTE: No .wrap() here - content is already pre-wrapped by render_transcript()
    // Adding wrap would cause double-wrapping and visual artifacts
    let transcript = Paragraph::new(visible_lines).block(Block::default().borders(Borders::NONE));
    let transcript_area = Rect {
        x: chunks[0].x + TRANSCRIPT_MARGIN,
        y: chunks[0].y,
        width: chunks[0].width.saturating_sub(TRANSCRIPT_MARGIN * 2),
        height: chunks[0].height,
    };
    frame.render_widget(transcript, transcript_area);

    // Input area with model on top-left border and path on bottom-right
    input::render_input(state, frame, chunks[1]);

    // Status line below input
    render_status_line(state, frame, chunks[2]);

    // Render overlay (last, so it appears on top)
    if let Some(overlay) = &app.overlay {
        overlay.render(frame, area, chunks[1].y);
    }
}

/// Renders the status line below the input.
fn render_status_line(state: &TuiState, frame: &mut Frame, area: Rect) {
    let spinner_idx = (state.spinner_frame / SPINNER_SPEED_DIVISOR) % SPINNER_FRAMES.len();
    let spinner = SPINNER_FRAMES[spinner_idx];

    let spans: Vec<Span> = match &state.agent_state {
        AgentState::Idle => {
            // Show helpful shortcuts when idle
            vec![
                Span::styled("Ctrl+P", Style::default().fg(Color::DarkGray)),
                Span::raw(" commands  "),
                Span::styled("Ctrl+C", Style::default().fg(Color::DarkGray)),
                Span::raw(" quit"),
            ]
        }
        AgentState::Waiting { .. } => {
            vec![
                Span::styled(spinner, Style::default().fg(Color::Yellow)),
                Span::raw(" "),
                Span::styled("Waiting...", Style::default().fg(Color::Yellow)),
                Span::raw("  "),
                Span::styled("Esc", Style::default().fg(Color::DarkGray)),
                Span::raw(" to cancel"),
            ]
        }
        AgentState::Streaming { .. } => {
            vec![
                Span::styled(spinner, Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled("Streaming...", Style::default().fg(Color::Cyan)),
                Span::raw("  "),
                Span::styled("Esc", Style::default().fg(Color::DarkGray)),
                Span::raw(" to cancel"),
            ]
        }
    };

    let status = Paragraph::new(Line::from(spans)).alignment(Alignment::Left);
    frame.render_widget(status, area);
}

/// Renders the transcript into ratatui Lines.
///
/// Also builds the position map for selection coordinate translation.
///
/// Returns (lines, is_lazy) where is_lazy indicates if lazy rendering was used.
/// When lazy rendering is used, lines are already scrolled and ready to display.
fn render_transcript(state: &TuiState, width: usize) -> (Vec<Line<'static>>, bool) {
    // Try lazy rendering if we have cell line info
    if let Some(visible) = state
        .transcript
        .scroll
        .visible_range(state.transcript.viewport_height)
    {
        return (render_transcript_lazy(state, width, visible), true);
    }

    // Fall back to full rendering (first frame or after changes)
    (render_transcript_full(state, width), false)
}

/// Full transcript rendering - iterates all cells.
///
/// Used on first frame or when cell_line_info needs to be rebuilt.
fn render_transcript_full(state: &TuiState, width: usize) -> Vec<Line<'static>> {
    use unicode_segmentation::UnicodeSegmentation;

    let mut lines = Vec::new();

    // Clear and rebuild the position map
    state.transcript.position_map.clear();

    for cell in &state.transcript.cells {
        let styled_lines = cell.display_lines_cached(
            width,
            state.spinner_frame / SPINNER_SPEED_DIVISOR,
            &state.transcript.wrap_cache,
        );

        for styled_line in styled_lines {
            // Build the line text for position mapping
            let line_text: String = styled_line.spans.iter().map(|s| s.text.as_str()).collect();
            let grapheme_count = line_text.graphemes(true).count();

            // Add to position map
            state
                .transcript
                .position_map
                .push(LineMapping { text: line_text });

            // Convert and add the line
            let line_idx = lines.len();
            let converted = convert_styled_line_with_selection(
                styled_line,
                &state.transcript.selection,
                line_idx,
                grapheme_count,
            );
            lines.push(converted);
        }

        // Add blank line between cells (also tracked in position map)
        state.transcript.position_map.push(LineMapping {
            text: String::new(),
        });
        lines.push(Line::default());
    }

    lines
}

/// Lazy transcript rendering - only renders visible cells.
///
/// Uses the pre-calculated visible range to skip off-screen cells.
/// Returns lines ready for display (already scrolled/sliced).
/// Position map is built for visible lines with offset tracking for selection.
fn render_transcript_lazy(
    state: &TuiState,
    width: usize,
    visible: crate::modes::tui::state::VisibleRange,
) -> Vec<Line<'static>> {
    use unicode_segmentation::UnicodeSegmentation;

    let mut lines = Vec::new();

    // Clear position map and set scroll offset for lazy mode
    state.transcript.position_map.clear();
    state
        .transcript
        .position_map
        .set_scroll_offset(visible.lines_before);

    // Track global line index for selection highlighting
    // This is the line index in the full transcript
    let mut global_line_idx = visible.lines_before;

    for (cell_idx, cell) in state.transcript.cells[visible.cell_range.clone()]
        .iter()
        .enumerate()
    {
        let styled_lines = cell.display_lines_cached(
            width,
            state.spinner_frame / SPINNER_SPEED_DIVISOR,
            &state.transcript.wrap_cache,
        );

        // For first cell, skip lines that are above viewport
        let skip_count = if cell_idx == 0 {
            visible.first_cell_line_offset
        } else {
            0
        };

        for (line_in_cell, styled_line) in styled_lines.into_iter().enumerate() {
            if line_in_cell < skip_count {
                // Don't increment global_line_idx here - it's already set correctly
                // to visible.lines_before which accounts for all skipped lines
                continue;
            }

            // Build the line text for position mapping
            let line_text: String = styled_line.spans.iter().map(|s| s.text.as_str()).collect();
            let grapheme_count = line_text.graphemes(true).count();

            // Add to position map - stores text for selection extraction
            state
                .transcript
                .position_map
                .push(LineMapping { text: line_text });

            // Convert with global line index for selection highlighting
            let converted = convert_styled_line_with_selection(
                styled_line,
                &state.transcript.selection,
                global_line_idx,
                grapheme_count,
            );
            lines.push(converted);
            global_line_idx += 1;
        }

        // Add blank line after each cell (matching full render behavior)
        // This keeps line counts consistent between full and lazy render
        state.transcript.position_map.push(LineMapping {
            text: String::new(),
        });
        lines.push(Line::default());
        global_line_idx += 1;
    }

    lines
}

/// Converts a transcript StyledLine to a ratatui Line.
fn convert_styled_line(styled_line: StyledLine) -> Line<'static> {
    let spans: Vec<Span<'static>> = styled_line
        .spans
        .into_iter()
        .map(|s| {
            let style = convert_style(s.style);
            Span::styled(s.text, style)
        })
        .collect();
    Line::from(spans)
}

/// Converts a StyledLine to a ratatui Line with selection highlighting.
///
/// If the line (at `line_idx`) is within the selection range, the selected
/// portion is rendered with a reversed background.
fn convert_styled_line_with_selection(
    styled_line: StyledLine,
    selection: &SelectionState,
    line_idx: usize,
    grapheme_count: usize,
) -> Line<'static> {
    use unicode_segmentation::UnicodeSegmentation;

    // Check if this line has any selection
    let Some((sel_start, sel_end)) = selection.line_selection(line_idx, grapheme_count) else {
        // No selection on this line, use normal rendering
        return convert_styled_line(styled_line);
    };

    // Build spans with selection highlighting
    let mut result_spans: Vec<Span<'static>> = Vec::new();
    let mut current_grapheme = 0usize;

    for span in styled_line.spans {
        let span_graphemes: Vec<&str> = span.text.graphemes(true).collect();
        let span_len = span_graphemes.len();
        let span_end = current_grapheme + span_len;

        let base_style = convert_style(span.style);
        let selected_style = base_style.add_modifier(Modifier::REVERSED);

        // Check overlap with selection
        let overlap_start = sel_start.max(current_grapheme);
        let overlap_end = sel_end.min(span_end);

        if overlap_start >= overlap_end {
            // No overlap with selection
            result_spans.push(Span::styled(span.text, base_style));
        } else {
            // Partial or full overlap - split the span
            let rel_start = overlap_start - current_grapheme;
            let rel_end = overlap_end - current_grapheme;

            // Before selection
            if rel_start > 0 {
                let before: String = span_graphemes[..rel_start].join("");
                result_spans.push(Span::styled(before, base_style));
            }

            // Selected portion
            let selected: String = span_graphemes[rel_start..rel_end].join("");
            result_spans.push(Span::styled(selected, selected_style));

            // After selection
            if rel_end < span_len {
                let after: String = span_graphemes[rel_end..].join("");
                result_spans.push(Span::styled(after, base_style));
            }
        }

        current_grapheme = span_end;
    }

    Line::from(result_spans)
}

/// Converts a transcript Style to a ratatui Style.
fn convert_style(style: TranscriptStyle) -> Style {
    match style {
        TranscriptStyle::Plain => Style::default(),
        TranscriptStyle::UserPrefix => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        TranscriptStyle::User => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::ITALIC),
        TranscriptStyle::Assistant => Style::default().fg(Color::White),
        TranscriptStyle::StreamingCursor => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::SLOW_BLINK),
        TranscriptStyle::SystemPrefix => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        TranscriptStyle::System => Style::default().fg(Color::DarkGray),
        TranscriptStyle::ToolBracket => Style::default().fg(Color::Gray),
        TranscriptStyle::ToolStatus => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        TranscriptStyle::ToolError => Style::default().fg(Color::Red),
        TranscriptStyle::ToolRunning => Style::default().fg(Color::Cyan),
        TranscriptStyle::ToolSuccess => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        TranscriptStyle::ToolCancelled => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::CROSSED_OUT | Modifier::BOLD),
        TranscriptStyle::ToolOutput => Style::default().fg(Color::DarkGray),
        TranscriptStyle::ThinkingPrefix => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::DIM),
        TranscriptStyle::Thinking => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM | Modifier::ITALIC),
        TranscriptStyle::Interrupted => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),

        // Markdown styles
        TranscriptStyle::CodeInline => Style::default().fg(Color::Cyan),
        TranscriptStyle::CodeBlock => Style::default().fg(Color::Cyan),
        TranscriptStyle::CodeFence => Style::default().fg(Color::DarkGray),
        TranscriptStyle::Emphasis => Style::default().add_modifier(Modifier::ITALIC),
        TranscriptStyle::Strong => Style::default().add_modifier(Modifier::BOLD),
        TranscriptStyle::H1 => Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        TranscriptStyle::H2 => Style::default().add_modifier(Modifier::BOLD),
        TranscriptStyle::H3 => Style::default()
            .add_modifier(Modifier::ITALIC)
            .fg(Color::White),
        TranscriptStyle::Link => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::UNDERLINED),
        TranscriptStyle::BlockQuote => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::ITALIC),
        TranscriptStyle::ListBullet => Style::default().fg(Color::Yellow),
        TranscriptStyle::ListNumber => Style::default().fg(Color::Yellow),
    }
}

/// Calculates the available height for the transcript given the terminal height and state.
/// Encapsulates layout logic so callers don't need to know about input/status heights.
pub fn calculate_transcript_height_with_state(state: &TuiState, terminal_height: u16) -> usize {
    let input_height = input::calculate_input_height(state, terminal_height);
    terminal_height.saturating_sub(input_height + STATUS_HEIGHT) as usize
}

/// Calculates cell line info and returns it for external application.
///
/// Returns a Vec of (CellId, line_count) tuples that can be used to
/// update ScrollState::cell_line_info.
pub fn calculate_cell_line_counts(
    state: &TuiState,
    terminal_width: usize,
) -> Vec<(crate::modes::tui::transcript::CellId, usize)> {
    let effective_width = terminal_width.saturating_sub((TRANSCRIPT_MARGIN * 2) as usize);

    state
        .transcript
        .cells
        .iter()
        .map(|cell| {
            let lines = cell.display_lines_cached(
                effective_width,
                state.spinner_frame / SPINNER_SPEED_DIVISOR,
                &state.transcript.wrap_cache,
            );
            // +1 for blank line between cells
            (cell.id(), lines.len() + 1)
        })
        .collect()
}
