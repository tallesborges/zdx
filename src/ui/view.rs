//! Pure view/render functions for the TUI.
//!
//! This module contains all rendering logic. Functions here:
//! - Take `&TuiState` by immutable reference
//! - Draw to a ratatui Frame
//! - Never mutate state or return effects
//!
//! The separation from TuiRuntime eliminates borrow-checker conflicts
//! that previously required cloning state for rendering.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::ui::overlays::{render_command_palette, render_login_overlay, render_model_picker};
use crate::ui::state::{AuthType, EngineState, OverlayState, TuiState};
use crate::ui::transcript::{Style as TranscriptStyle, StyledLine};

/// Height of the input area (lines).
pub const INPUT_HEIGHT: u16 = 5;

/// Height of header area (lines: title + status + border).
pub const HEADER_HEIGHT: u16 = 3;

/// Spinner speed divisor (render frames per spinner frame).
/// At 30fps render rate, 3 gives ~10fps spinner animation.
const SPINNER_SPEED_DIVISOR: usize = 3;

/// Renders the entire TUI to the frame.
///
/// This is a pure render function - it only reads state and draws to frame.
/// No mutations, no side effects.
pub fn view(state: &TuiState, frame: &mut Frame) {
    let area = frame.area();

    // Get terminal size for transcript rendering
    let transcript_width = area.width.saturating_sub(2) as usize;

    // Calculate transcript pane height
    let transcript_height = area.height.saturating_sub(HEADER_HEIGHT + INPUT_HEIGHT) as usize;

    // Pre-render transcript lines
    let all_lines = render_transcript(state, transcript_width);
    let total_lines = all_lines.len();

    // Use ScrollState for offset calculation (uses cached line count)
    // Note: We use total_lines here since we just calculated it
    let scroll_offset = {
        // Temporarily use the fresh line count for accurate offset calculation
        let max_offset = total_lines.saturating_sub(transcript_height);
        if state.scroll.is_following() {
            total_lines.saturating_sub(transcript_height)
        } else {
            state.scroll.get_offset(transcript_height).min(max_offset)
        }
    };

    // Check if there's content below the viewport (for indicator)
    let has_content_below = scroll_offset + transcript_height < total_lines;

    // Slice visible lines
    let visible_end = (scroll_offset + transcript_height).min(total_lines);
    let visible_lines: Vec<Line<'static>> = all_lines
        .into_iter()
        .skip(scroll_offset)
        .take(visible_end - scroll_offset)
        .collect();

    // Create layout: header, transcript, input
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(HEADER_HEIGHT), // Header
            Constraint::Min(1),                // Transcript
            Constraint::Length(INPUT_HEIGHT),  // Input
        ])
        .split(area);

    // Render header
    render_header(state, frame, chunks[0], has_content_below);

    // Transcript area (already sliced to visible)
    let transcript = Paragraph::new(visible_lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::NONE));
    frame.render_widget(transcript, chunks[1]);

    // Input area
    frame.render_widget(&state.textarea, chunks[2]);

    // Render overlay (last, so it appears on top)
    match &state.overlay {
        OverlayState::CommandPalette(palette) => {
            render_command_palette(frame, palette, area, chunks[2].y);
        }
        OverlayState::ModelPicker(picker) => {
            render_model_picker(frame, picker, area, chunks[2].y);
        }
        OverlayState::Login(login_state) => {
            render_login_overlay(frame, login_state, area);
        }
        OverlayState::None => {}
    }
}

/// Renders the header section (title + status line).
fn render_header(state: &TuiState, frame: &mut Frame, area: Rect, has_content_below: bool) {
    // Header line 1: Title and scroll indicator
    let mut title_spans = vec![
        Span::styled(
            "ZDX",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" — "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" to quit"),
    ];
    if has_content_below {
        title_spans.push(Span::raw("  "));
        title_spans.push(Span::styled("▼ more", Style::default().fg(Color::DarkGray)));
    }

    // Header line 2: Status line
    let status_state = match &state.engine_state {
        EngineState::Idle => ("Ready", Color::Green),
        EngineState::Waiting { .. } => ("Thinking...", Color::Yellow),
        EngineState::Streaming { .. } => ("Streaming...", Color::Yellow),
    };

    let auth_indicator = match state.auth_type {
        AuthType::OAuth => ("●", Color::Green, "OAuth"),
        AuthType::ApiKey => ("●", Color::Blue, "API"),
        AuthType::None => ("○", Color::Red, "No Auth"),
    };

    let mut status_spans = vec![
        Span::styled(&state.config.model, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(auth_indicator.0, Style::default().fg(auth_indicator.1)),
        Span::styled(
            format!(" {}", auth_indicator.2),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" │ "),
        Span::styled(status_state.0, Style::default().fg(status_state.1)),
    ];

    if let Some(idx) = state.history_index {
        let total = state.command_history.len();
        status_spans.push(Span::raw(" │ "));
        status_spans.push(Span::styled(
            format!("history {}/{}", idx + 1, total),
            Style::default().fg(Color::DarkGray),
        ));
    }

    if matches!(state.overlay, OverlayState::CommandPalette(_)) {
        status_spans.push(Span::raw(" │ "));
        status_spans.push(Span::styled(
            "/ Commands (Esc to cancel)",
            Style::default().fg(Color::Yellow),
        ));
    }

    let header = Paragraph::new(vec![Line::from(title_spans), Line::from(status_spans)])
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
    frame.render_widget(header, area);
}

/// Renders the transcript into ratatui Lines.
fn render_transcript(state: &TuiState, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    for cell in &state.transcript {
        let styled_lines = cell.display_lines(width, state.spinner_frame / SPINNER_SPEED_DIVISOR);
        for styled_line in styled_lines {
            lines.push(convert_styled_line(styled_line));
        }
        // Add blank line between cells
        lines.push(Line::default());
    }

    // Remove trailing blank line if not waiting or streaming
    let is_active = matches!(
        state.engine_state,
        EngineState::Waiting { .. } | EngineState::Streaming { .. }
    );
    if !is_active && lines.last().map(|l| l.spans.is_empty()).unwrap_or(false) {
        lines.pop();
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
        TranscriptStyle::AssistantPrefix => Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
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
            .add_modifier(Modifier::CROSSED_OUT),
        TranscriptStyle::ToolOutput => Style::default().fg(Color::DarkGray),
    }
}

/// Returns the total line count from the transcript rendering.
/// Called after view() to update cached_line_count in state.
pub fn calculate_line_count(state: &TuiState, width: usize) -> usize {
    render_transcript(state, width).len()
}
