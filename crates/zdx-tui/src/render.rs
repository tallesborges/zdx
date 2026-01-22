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

use crate::common::text::truncate_with_ellipsis;
use crate::common::{Scrollbar, TaskKind};
use crate::input;
use crate::overlays::OverlayExt;
use crate::state::{AgentState, AppState, TuiState};
use crate::statusline::render_debug_status_line;
use crate::transcript::{self, CellId};

/// Height of status line below input.
const STATUS_HEIGHT: u16 = 1;

/// Height of debug status line (when enabled).
const DEBUG_STATUS_HEIGHT: u16 = 1;

/// Max queued prompts to display in the queue panel.
const QUEUE_MAX_ITEMS: usize = 3;

/// Horizontal margin for the transcript area (left and right).
/// Transcript horizontal margin (padding on each side).
pub const TRANSCRIPT_MARGIN: u16 = 1;

/// Width reserved for the scrollbar on the right side.
/// This ensures there's always a gap between transcript content and the scrollbar.
const SCROLLBAR_WIDTH: u16 = 1;

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
    let queue_summaries = state.input.queued_summaries(QUEUE_MAX_ITEMS);
    let queue_total = state.input.queued.len();
    let queue_height = if queue_summaries.is_empty() {
        0
    } else {
        queue_summaries.len() as u16 + 2
    };

    // Debug status line height (only when enabled)
    let debug_status_height = if state.show_debug_status {
        DEBUG_STATUS_HEIGHT
    } else {
        0
    };

    // Get terminal size for transcript rendering (account for margins and scrollbar)
    let transcript_width =
        area.width
            .saturating_sub(TRANSCRIPT_MARGIN * 2 + SCROLLBAR_WIDTH) as usize;

    // Calculate transcript pane height (no header now)
    let transcript_height = area
        .height
        .saturating_sub(input_height + STATUS_HEIGHT + queue_height + debug_status_height)
        as usize;

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

    // Create layout: transcript, queue, input, status, [debug status]
    let constraints = if state.show_debug_status {
        vec![
            Constraint::Min(1),                      // Transcript
            Constraint::Length(queue_height),        // Queue summary
            Constraint::Length(input_height),        // Input (dynamic)
            Constraint::Length(STATUS_HEIGHT),       // Status line
            Constraint::Length(debug_status_height), // Debug status line
        ]
    } else {
        vec![
            Constraint::Min(1),                // Transcript
            Constraint::Length(queue_height),  // Queue summary
            Constraint::Length(input_height),  // Input (dynamic)
            Constraint::Length(STATUS_HEIGHT), // Status line
        ]
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    // Transcript area with horizontal margins (also accounts for scrollbar)
    // NOTE: No .wrap() here - content is already pre-wrapped by render_transcript()
    // Adding wrap would cause double-wrapping and visual artifacts
    let transcript = Paragraph::new(visible_lines).block(Block::default().borders(Borders::NONE));
    let transcript_area = Rect {
        x: chunks[0].x + TRANSCRIPT_MARGIN,
        y: chunks[0].y,
        width: chunks[0]
            .width
            .saturating_sub(TRANSCRIPT_MARGIN * 2 + SCROLLBAR_WIDTH),
        height: chunks[0].height,
    };
    frame.render_widget(transcript, transcript_area);

    // Render scrollbar if there's content to scroll
    // We recalculate offset here to ensure it matches total_lines (which might be fresher than
    // the cached state during streaming/lazy-render fallback)
    let scroll_offset = if state.transcript.scroll.is_following() {
        total_lines.saturating_sub(transcript_height)
    } else {
        let max_offset = total_lines.saturating_sub(transcript_height);
        state
            .transcript
            .scroll
            .get_offset(transcript_height)
            .min(max_offset)
    };

    frame.render_widget(
        Scrollbar::new(total_lines, transcript_height, scroll_offset),
        chunks[0],
    );

    // Input area with model on top-left border and path on bottom-right
    if queue_height > 0 {
        render_queue_panel(frame, chunks[1], &queue_summaries, queue_total);
    }

    // Input area with model on top-left border and path on bottom-right
    input::render_input(state, frame, chunks[2]);

    // Status line below input
    render_status_line(state, frame, chunks[3]);

    // Debug status line (when enabled)
    if state.show_debug_status {
        let status_line = state.status_line.snapshot();
        render_debug_status_line(&status_line, frame, chunks[4]);
    }

    // Render overlay (last, so it appears on top)
    app.overlay.render(frame, area, chunks[2].y);
}

/// Formats a duration for the status line display.
fn format_elapsed(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        let mins = secs / 60;
        let remaining_secs = secs % 60;
        format!("{}m{:02}s", mins, remaining_secs)
    } else {
        format!("{}s", secs)
    }
}

/// Renders the status line below the input.
fn render_status_line(state: &TuiState, frame: &mut Frame, area: Rect) {
    let spinner_idx =
        (state.spinner_frame / transcript::SPINNER_SPEED_DIVISOR) % SPINNER_FRAMES.len();
    let spinner = SPINNER_FRAMES[spinner_idx];

    // Get turn elapsed time for display
    let elapsed = state.status_line.snapshot().turn_elapsed;
    let elapsed_span = elapsed.map(|d| format!(" ({})", format_elapsed(d)));

    // Check for bash execution first (takes priority over idle state)
    let spans: Vec<Span> = if state.tasks.state(TaskKind::Bash).is_running() {
        let mut spans = vec![
            Span::styled(spinner, Style::default().fg(Color::Green)),
            Span::raw(" "),
            Span::styled("Running bash...", Style::default().fg(Color::Green)),
        ];
        if let Some(ref elapsed) = elapsed_span {
            spans.push(Span::styled(
                elapsed.clone(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        spans.extend([
            Span::raw("  "),
            Span::styled("Esc", Style::default().fg(Color::DarkGray)),
            Span::raw(" to cancel"),
        ]);
        spans
    } else {
        match &state.agent_state {
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
                let mut spans = vec![
                    Span::styled(spinner, Style::default().fg(Color::Yellow)),
                    Span::raw(" "),
                    Span::styled("Waiting...", Style::default().fg(Color::Yellow)),
                ];
                if let Some(ref elapsed) = elapsed_span {
                    spans.push(Span::styled(
                        elapsed.clone(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                spans.extend([
                    Span::raw("  "),
                    Span::styled("Esc", Style::default().fg(Color::DarkGray)),
                    Span::raw(" to cancel"),
                ]);
                spans
            }
            AgentState::Streaming { .. } => {
                let mut spans = vec![
                    Span::styled(spinner, Style::default().fg(Color::Cyan)),
                    Span::raw(" "),
                    Span::styled("Streaming...", Style::default().fg(Color::Cyan)),
                ];
                if let Some(ref elapsed) = elapsed_span {
                    spans.push(Span::styled(
                        elapsed.clone(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                spans.extend([
                    Span::raw("  "),
                    Span::styled("Esc", Style::default().fg(Color::DarkGray)),
                    Span::raw(" to cancel"),
                ]);
                spans
            }
        }
    };

    let status = Paragraph::new(Line::from(spans)).alignment(Alignment::Left);
    frame.render_widget(status, area);
}

/// Renders the queued prompt summary panel between transcript and input.
fn render_queue_panel(frame: &mut Frame, area: Rect, summaries: &[String], total: usize) {
    if summaries.is_empty() || area.height == 0 {
        return;
    }

    // Inner width accounts for borders (2) + bullet prefix "- " (2)
    let inner_width = area.width.saturating_sub(4) as usize;
    let bullet_style = Style::default().fg(Color::DarkGray);
    let text_style = Style::default().fg(Color::Gray);

    let lines: Vec<Line<'static>> = summaries
        .iter()
        .map(|line| {
            // Use unicode-aware truncation for proper handling of wide characters
            let text = truncate_with_ellipsis(line, inner_width);
            Line::from(vec![
                Span::styled("- ", bullet_style),
                Span::styled(text, text_style),
            ])
        })
        .collect();

    let title = format!(" Queued ({}) ", total);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(Span::styled(title, bullet_style)));
    let panel = Paragraph::new(lines).block(block);
    frame.render_widget(panel, area);
}

/// Calculates the available height for the transcript given the terminal height and state.
/// Encapsulates layout logic so callers don't need to know about input/status heights.
pub fn calculate_transcript_height_with_state(state: &TuiState, terminal_height: u16) -> usize {
    let input_height = input::calculate_input_height(state, terminal_height);
    let queue_height = if state.input.has_queued() {
        (state.input.queued_summaries(QUEUE_MAX_ITEMS).len() as u16).saturating_add(2)
    } else {
        0
    };
    let debug_status_height = if state.show_debug_status {
        DEBUG_STATUS_HEIGHT
    } else {
        0
    };
    terminal_height
        .saturating_sub(input_height + STATUS_HEIGHT + queue_height + debug_status_height)
        as usize
}

/// Calculates cell line info and returns it for external application.
///
/// This is a thin wrapper around transcript::calculate_cell_line_counts
/// that passes the combined horizontal overhead (margins + scrollbar).
pub fn calculate_cell_line_counts(state: &TuiState, terminal_width: usize) -> Vec<(CellId, usize)> {
    let horizontal_overhead = (TRANSCRIPT_MARGIN * 2 + SCROLLBAR_WIDTH) as usize;
    transcript::calculate_cell_line_counts(state, terminal_width, horizontal_overhead)
}
