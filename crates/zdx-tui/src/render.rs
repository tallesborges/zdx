//! Pure view/render functions for the TUI.
//!
//! This module contains all rendering logic. Functions here:
//! - Take `&AppState` by immutable reference
//! - Draw to a ratatui Frame
//! - Never mutate state or return effects
//!
//! The separation from `TuiRuntime` eliminates borrow-checker conflicts
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
    let metrics = compute_render_metrics(state, area);
    let (visible_lines, total_lines, scroll_offset) =
        build_visible_transcript_lines(state, metrics.transcript_width, metrics.transcript_height);
    let chunks = split_main_layout(area, &metrics, state.show_debug_status);

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

    frame.render_widget(
        Scrollbar::new(total_lines, metrics.transcript_height, scroll_offset),
        chunks[0],
    );

    // Input area with model on top-left border and path on bottom-right
    if metrics.queue_height > 0 {
        render_queue_panel(
            frame,
            chunks[1],
            &metrics.queue_summaries,
            metrics.queue_total,
        );
    }

    // Input area — hide cursor when an overlay is covering the screen
    let show_input_cursor = app.overlay.is_none();
    input::render_input_with_cursor(state, frame, chunks[2], show_input_cursor);
    state.input_area.set(chunks[2]);

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

struct RenderMetrics {
    input_height: u16,
    queue_summaries: Vec<String>,
    queue_total: usize,
    queue_height: u16,
    transcript_width: usize,
    transcript_height: usize,
}

fn compute_render_metrics(state: &TuiState, area: Rect) -> RenderMetrics {
    let input_height = input::calculate_input_height(state, area.height);
    let queue_summaries = state.input.queued_summaries(QUEUE_MAX_ITEMS);
    let queue_total = state.input.queued.len();
    let queue_height = if queue_summaries.is_empty() {
        0
    } else {
        queue_summaries.len() as u16 + 2
    };
    let debug_status_height = if state.show_debug_status {
        DEBUG_STATUS_HEIGHT
    } else {
        0
    };
    let transcript_width =
        area.width
            .saturating_sub(TRANSCRIPT_MARGIN * 2 + SCROLLBAR_WIDTH) as usize;
    let transcript_height = area
        .height
        .saturating_sub(input_height + STATUS_HEIGHT + queue_height + debug_status_height)
        as usize;

    RenderMetrics {
        input_height,
        queue_summaries,
        queue_total,
        queue_height,
        transcript_width,
        transcript_height,
    }
}

fn split_main_layout(area: Rect, metrics: &RenderMetrics, show_debug_status: bool) -> Vec<Rect> {
    let constraints = if show_debug_status {
        vec![
            Constraint::Min(1),
            Constraint::Length(metrics.queue_height),
            Constraint::Length(metrics.input_height),
            Constraint::Length(STATUS_HEIGHT),
            Constraint::Length(DEBUG_STATUS_HEIGHT),
        ]
    } else {
        vec![
            Constraint::Min(1),
            Constraint::Length(metrics.queue_height),
            Constraint::Length(metrics.input_height),
            Constraint::Length(STATUS_HEIGHT),
        ]
    };

    Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area)
        .to_vec()
}

fn build_visible_transcript_lines(
    state: &TuiState,
    transcript_width: usize,
    transcript_height: usize,
) -> (Vec<Line<'static>>, usize, usize) {
    let (all_lines, is_lazy) = transcript::render_transcript(state, transcript_width);
    let total_lines = if is_lazy {
        state.transcript.scroll.cached_line_count
    } else {
        all_lines.len()
    };
    let scroll_offset = compute_scroll_offset(state, total_lines, transcript_height);
    let visible = if is_lazy {
        all_lines
    } else {
        let visible_end = (scroll_offset + transcript_height).min(total_lines);
        all_lines
            .into_iter()
            .skip(scroll_offset)
            .take(visible_end - scroll_offset)
            .collect()
    };

    (
        bottom_align_lines(visible, transcript_height),
        total_lines,
        scroll_offset,
    )
}

fn compute_scroll_offset(state: &TuiState, total_lines: usize, transcript_height: usize) -> usize {
    if state.transcript.scroll.is_following() {
        total_lines.saturating_sub(transcript_height)
    } else {
        let max_offset = total_lines.saturating_sub(transcript_height);
        state
            .transcript
            .scroll
            .get_offset(transcript_height)
            .min(max_offset)
    }
}

fn bottom_align_lines(lines: Vec<Line<'static>>, transcript_height: usize) -> Vec<Line<'static>> {
    if lines.len() >= transcript_height {
        return lines;
    }

    let mut padded = vec![Line::default(); transcript_height - lines.len()];
    padded.extend(lines);
    padded
}

/// Formats a duration for the status line display.
fn format_elapsed(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        let mins = secs / 60;
        let remaining_secs = secs % 60;
        format!("{mins}m{remaining_secs:02}s")
    } else {
        format!("{secs}s")
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
                    Span::styled("Ctrl+O", Style::default().fg(Color::DarkGray)),
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

    let title = format!(" Queued ({total}) ");
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
/// This is a thin wrapper around `transcript::calculate_cell_line_counts`
/// that passes the combined horizontal overhead (margins + scrollbar).
pub fn calculate_cell_line_counts(state: &TuiState, terminal_width: usize) -> Vec<(CellId, usize)> {
    let horizontal_overhead = (TRANSCRIPT_MARGIN * 2 + SCROLLBAR_WIDTH) as usize;
    transcript::calculate_cell_line_counts(state, terminal_width, horizontal_overhead)
}
