//! Pure view/render functions for the TUI.
//!
//! This module contains all rendering logic. Functions here:
//! - Take `&TuiState` by immutable reference
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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::models::ModelOption;
use crate::ui::overlays::{
    render_command_palette, render_login_overlay, render_model_picker, render_thinking_picker,
};
use crate::ui::state::{AgentState, AuthType, OverlayState, SessionUsage, TuiState};
use crate::ui::transcript::{Style as TranscriptStyle, StyledLine};

/// Minimum height of the input area (lines, including borders).
const INPUT_HEIGHT_MIN: u16 = 5;

/// Maximum height of the input area as a percentage of screen height.
const INPUT_HEIGHT_MAX_PERCENT: f32 = 0.4;

/// Height of status line below input.
const STATUS_HEIGHT: u16 = 1;

/// Horizontal margin for the transcript area (left and right).
const TRANSCRIPT_MARGIN: u16 = 1;

/// Spinner frames for status line animation.
const SPINNER_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

/// Spinner speed divisor (render frames per spinner frame).
const SPINNER_SPEED_DIVISOR: usize = 6;

/// Calculates the dynamic input height based on content and terminal size.
///
/// - Minimum: INPUT_HEIGHT_MIN (5 lines with borders)
/// - Maximum: 40% of terminal height
/// - Expands when content has more than 3 lines
fn calculate_input_height(state: &TuiState, terminal_height: u16) -> u16 {
    let line_count = state.textarea.lines().len() as u16;

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

/// Renders the entire TUI to the frame.
///
/// This is a pure render function - it only reads state and draws to frame.
/// No mutations, no side effects.
pub fn view(state: &TuiState, frame: &mut Frame) {
    let area = frame.area();

    // Calculate dynamic input height based on content
    let input_height = calculate_input_height(state, area.height);

    // Get terminal size for transcript rendering (account for margins)
    let transcript_width = area.width.saturating_sub(TRANSCRIPT_MARGIN * 2) as usize;

    // Calculate transcript pane height (no header now)
    let transcript_height = area.height.saturating_sub(input_height + STATUS_HEIGHT) as usize;

    // Pre-render transcript lines
    let all_lines = render_transcript(state, transcript_width);
    let total_lines = all_lines.len();

    // Use ScrollState for offset calculation (uses cached line count)
    let scroll_offset = {
        let max_offset = total_lines.saturating_sub(transcript_height);
        if state.transcript.scroll.is_following() {
            total_lines.saturating_sub(transcript_height)
        } else {
            state.transcript.scroll.get_offset(transcript_height).min(max_offset)
        }
    };

    // Slice visible lines
    let visible_end = (scroll_offset + transcript_height).min(total_lines);
    let content_lines: Vec<Line<'static>> = all_lines
        .into_iter()
        .skip(scroll_offset)
        .take(visible_end - scroll_offset)
        .collect();

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
    render_input(state, frame, chunks[1]);

    // Status line below input
    render_status_line(state, frame, chunks[2]);

    // Render overlay (last, so it appears on top)
    match &state.overlay {
        OverlayState::CommandPalette(palette) => {
            render_command_palette(frame, palette, area, chunks[1].y);
        }
        OverlayState::ModelPicker(picker) => {
            render_model_picker(frame, picker, area, chunks[1].y);
        }
        OverlayState::ThinkingPicker(picker) => {
            render_thinking_picker(frame, picker, area, chunks[1].y);
        }
        OverlayState::Login(login_state) => {
            render_login_overlay(frame, login_state, area);
        }
        OverlayState::None => {}
    }
}

/// Renders the input area with model info on top border and path on bottom border.
fn render_input(state: &TuiState, frame: &mut Frame, area: Rect) {
    use crate::config::ThinkingLevel;

    // Build top-left title: model name + auth type + thinking level
    let auth_indicator = match state.auth_type {
        AuthType::OAuth => " (oauth)",
        AuthType::ApiKey => " (api-key)",
        AuthType::None => "",
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

    // Add thinking indicator with dim style (only when enabled)
    if state.config.thinking_level != ThinkingLevel::Off {
        title_spans.push(Span::styled(
            format!(" [{}]", state.config.thinking_level.display_name()),
            thinking_style,
        ));
    }

    title_spans.push(Span::styled(" ", base_style));

    // Build top-right title: AMP-style usage display
    // Format: "{percentage}% of {context} · ${cost} (cached: ${savings})"
    let usage_spans = build_usage_display(&state.usage, &state.config.model);

    // Build bottom-left title: detailed token breakdown
    let token_spans = build_token_breakdown(&state.usage);

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

    let (cursor_line, cursor_col) = state.textarea.cursor();
    let cursor_line = cursor_line.min(state.textarea.lines().len().saturating_sub(1));

    // Manually wrap lines at exact character widths (not word boundaries)
    // This ensures cursor calculation matches the actual rendering
    let mut wrapped_lines: Vec<Line> = Vec::new();
    let mut visual_row = 0usize;
    let mut cursor_visual_row = 0usize;
    let mut cursor_visual_col = 0usize;

    for (line_idx, logical_line) in state.textarea.lines().iter().enumerate() {
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
            let row_offset = width_to_cursor / available_width;
            let col_offset = width_to_cursor % available_width;
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

    // Calculate vertical scroll offset to keep cursor visible
    let total_visual_rows = wrapped_lines.len();
    let viewport_height = inner_area.height as usize;

    let scroll_offset = if total_visual_rows <= viewport_height {
        // All content fits, no scrolling needed
        0
    } else {
        // Content doesn't fit, calculate scroll to show cursor
        // Keep cursor in the middle third of the viewport when possible
        let ideal_cursor_position = viewport_height / 2;

        if cursor_visual_row < ideal_cursor_position {
            // Near top, show from beginning
            0
        } else if cursor_visual_row >= total_visual_rows.saturating_sub(ideal_cursor_position) {
            // Near bottom, show the last viewport_height lines
            total_visual_rows.saturating_sub(viewport_height)
        } else {
            // In middle, center the cursor
            cursor_visual_row.saturating_sub(ideal_cursor_position)
        }
    };

    // Slice visible lines based on scroll offset
    let visible_lines: Vec<Line> = wrapped_lines
        .into_iter()
        .skip(scroll_offset)
        .take(viewport_height)
        .collect();

    let input_paragraph = Paragraph::new(visible_lines).block(block);
    frame.render_widget(input_paragraph, area);

    // Adjust cursor position by scroll offset
    let cursor_x = inner_area.x + cursor_visual_col as u16;
    let cursor_y = inner_area.y + (cursor_visual_row.saturating_sub(scroll_offset) as u16);

    if cursor_x < inner_area.x + inner_area.width && cursor_y < inner_area.y + inner_area.height {
        frame.set_cursor_position((cursor_x, cursor_y));
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

/// Builds the AMP-style usage display spans.
///
/// Format: "{percentage}% of {context} · ${cost} (cached)"
/// Example: "11% of 200k · $0.008 (cached)"
fn build_usage_display(usage: &SessionUsage, model_id: &str) -> Vec<Span<'static>> {
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
                        SessionUsage::format_context_limit(m.context_limit)
                    ),
                    usage_style,
                ),
                Span::styled(SessionUsage::format_cost(cost), cost_style),
            ];

            // Show cache savings indicator if there are cache hits
            if savings > 0.001 {
                spans.push(Span::styled(
                    format!(" (saved {})", SessionUsage::format_cost(savings)),
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
                Span::styled(SessionUsage::format_tokens(total), usage_style),
                Span::styled(" tokens ", usage_style),
            ]
        }
    }
}

/// Builds the detailed token breakdown for bottom-left display.
///
/// Format: "↑{input} ↓{output} R{cache_read} W{cache_write}"
fn build_token_breakdown(usage: &SessionUsage) -> Vec<Span<'static>> {
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
        Span::styled(SessionUsage::format_tokens(usage.input_tokens), label_style),
        Span::styled(" ↓", output_style),
        Span::styled(
            SessionUsage::format_tokens(usage.output_tokens),
            label_style,
        ),
        Span::styled(" R", cache_read_style),
        Span::styled(
            SessionUsage::format_tokens(usage.cache_read_tokens),
            label_style,
        ),
        Span::styled(" W", cache_write_style),
        Span::styled(
            format!("{} ", SessionUsage::format_tokens(usage.cache_write_tokens)),
            label_style,
        ),
    ]
}

/// Renders the transcript into ratatui Lines.
fn render_transcript(state: &TuiState, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    for cell in &state.transcript.cells {
        let styled_lines = cell.display_lines_cached(
            width,
            state.spinner_frame / SPINNER_SPEED_DIVISOR,
            &state.transcript.wrap_cache,
        );
        for styled_line in styled_lines {
            lines.push(convert_styled_line(styled_line));
        }
        // Add blank line between cells
        lines.push(Line::default());
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

/// Returns the total line count from the transcript rendering.
/// Called after view() to update cached_line_count in state.
/// Takes the raw terminal width - margins are applied internally.
pub fn calculate_line_count(state: &TuiState, terminal_width: usize) -> usize {
    let effective_width = terminal_width.saturating_sub((TRANSCRIPT_MARGIN * 2) as usize);
    render_transcript(state, effective_width).len()
}

/// Calculates the available height for the transcript given the terminal height and state.
/// Encapsulates layout logic so callers don't need to know about input/status heights.
pub fn calculate_transcript_height_with_state(state: &TuiState, terminal_height: u16) -> usize {
    let input_height = calculate_input_height(state, terminal_height);
    terminal_height.saturating_sub(input_height + STATUS_HEIGHT) as usize
}
