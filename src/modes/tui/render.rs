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
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::modes::tui::app::{AgentState, AppState, TuiState};
use crate::modes::tui::input;
use crate::modes::tui::overlays::OverlayExt;
use crate::modes::tui::transcript::{self, CellId};

/// Height of status line below input.
const STATUS_HEIGHT: u16 = 1;

/// Horizontal margin for the transcript area (left and right).
/// Transcript horizontal margin (padding on each side).
pub const TRANSCRIPT_MARGIN: u16 = 1;

/// Spinner frames for status line animation.
const SPINNER_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

/// Renders the entire TUI to the frame.
///
/// This is a pure render function - it only reads state and draws to frame.
/// No mutations, no side effects.
pub fn render(app: &AppState, frame: &mut Frame) {
    let area = frame.area();
    let state = &app.tui;

    // Calculate dynamic input height based on content
    let input_height = input::calculate_input_height(state, area.height);

    // Get terminal size for transcript rendering (account for margins)
    let transcript_width = area.width.saturating_sub(TRANSCRIPT_MARGIN * 2) as usize;

    // Calculate transcript pane height (no header now)
    let transcript_height = area.height.saturating_sub(input_height + STATUS_HEIGHT) as usize;

    // Pre-render transcript lines using transcript module
    // is_lazy indicates whether lazy rendering was used (lines are already scrolled)
    let (all_lines, is_lazy) = transcript::render_transcript(state, transcript_width);
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
    app.overlay.render(frame, area, chunks[1].y);
}

/// Renders the status line below the input.
fn render_status_line(state: &TuiState, frame: &mut Frame, area: Rect) {
    let spinner_idx =
        (state.spinner_frame / transcript::SPINNER_SPEED_DIVISOR) % SPINNER_FRAMES.len();
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

/// Calculates the available height for the transcript given the terminal height and state.
/// Encapsulates layout logic so callers don't need to know about input/status heights.
pub fn calculate_transcript_height_with_state(state: &TuiState, terminal_height: u16) -> usize {
    let input_height = input::calculate_input_height(state, terminal_height);
    terminal_height.saturating_sub(input_height + STATUS_HEIGHT) as usize
}

/// Calculates cell line info and returns it for external application.
///
/// This is a thin wrapper around transcript::calculate_cell_line_counts
/// that passes the TRANSCRIPT_MARGIN constant.
pub fn calculate_cell_line_counts(state: &TuiState, terminal_width: usize) -> Vec<(CellId, usize)> {
    transcript::calculate_cell_line_counts(state, terminal_width, TRANSCRIPT_MARGIN)
}
