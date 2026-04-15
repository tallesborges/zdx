//! Tool detail popup overlay.
//!
//! Displays full tool information in a near-full-screen popup:
//! args (pretty JSON), output, status, and error details.
//! Opens on click from compact tool header in transcript.
//! Supports live updates for running tools via render-time cell lookup.

use std::cell::Cell;
use std::fmt::Write as _;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::OverlayUpdate;
use super::render_utils::centered_rect;
use crate::transcript::{HistoryCell, SPINNER_SPEED_DIVISOR, ToolState};

/// Spinner frames for popup title animation.
const SPINNER_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

fn format_byte_truncation(stream: &str, total_bytes: u64) -> String {
    let size_str = if total_bytes >= 1024 * 1024 {
        format!("{:.1} MB", total_bytes as f64 / (1024.0 * 1024.0))
    } else if total_bytes >= 1024 {
        format!("{:.1} KB", total_bytes as f64 / 1024.0)
    } else {
        format!("{total_bytes} bytes")
    };
    format!("{stream} truncated: {size_str} total")
}

/// Builds human-readable output text for the popup.
fn build_popup_output_text(name: &str, data: &serde_json::Value) -> String {
    // Try stdout/stderr extraction (bash and other tools that produce it)
    let stdout = data.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
    let stderr = data.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
    if !stdout.is_empty() || !stderr.is_empty() {
        let mut text = String::new();
        if !stdout.is_empty() {
            text.push_str(stdout);
        }
        if !stderr.is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(stderr);
        }
        // Append metadata fields when present
        let metadata_keys = [
            "exit_code",
            "timed_out",
            "stdout_file",
            "stderr_file",
            "stdout_truncated",
            "stderr_truncated",
        ];
        let mut has_meta = false;
        for key in metadata_keys {
            if let Some(val) = data.get(key) {
                if !has_meta {
                    if !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str("───\n");
                    has_meta = true;
                }
                let _ = writeln!(text, "{key}: {val}");
            }
        }
        return text;
    }

    // For read tool: show file content directly
    if name == "read"
        && let Some(content) = data.get("content").and_then(serde_json::Value::as_str)
    {
        return content.to_string();
    }

    // For string results
    if let Some(text) = data.as_str() {
        return text.to_string();
    }

    // Fallback: pretty JSON
    serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string())
}

#[derive(Debug)]
pub struct ToolDetailState {
    pub tool_use_id: String,
    scroll_offset: Cell<usize>,
    /// True when user has manually scrolled; disables auto-scroll.
    user_scrolled: Cell<bool>,
}

impl ToolDetailState {
    pub fn open(tool_use_id: String) -> Self {
        Self {
            tool_use_id,
            scroll_offset: Cell::new(0),
            user_scrolled: Cell::new(false),
        }
    }

    /// Scroll up by `lines` (for mouse wheel).
    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset
            .set(self.scroll_offset.get().saturating_sub(lines));
        self.user_scrolled.set(true);
    }

    /// Scroll down by `lines` (for mouse wheel).
    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset
            .set(self.scroll_offset.get().saturating_add(lines));
        self.user_scrolled.set(true);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> OverlayUpdate {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => OverlayUpdate::close(),
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_offset
                    .set(self.scroll_offset.get().saturating_add(1));
                self.user_scrolled.set(true);
                OverlayUpdate::stay()
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_offset
                    .set(self.scroll_offset.get().saturating_sub(1));
                self.user_scrolled.set(true);
                OverlayUpdate::stay()
            }
            KeyCode::PageDown => {
                self.scroll_offset
                    .set(self.scroll_offset.get().saturating_add(20));
                self.user_scrolled.set(true);
                OverlayUpdate::stay()
            }
            KeyCode::PageUp => {
                self.scroll_offset
                    .set(self.scroll_offset.get().saturating_sub(20));
                self.user_scrolled.set(true);
                OverlayUpdate::stay()
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.scroll_offset.set(0);
                self.user_scrolled.set(true);
                OverlayUpdate::stay()
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.scroll_offset.set(usize::MAX); // clamped at render
                self.user_scrolled.set(false); // Re-enable auto-scroll
                OverlayUpdate::stay()
            }
            _ => OverlayUpdate::stay(),
        }
    }

    /// Render the tool detail popup. Receives the live cell from render orchestration.
    #[allow(clippy::too_many_lines)]
    pub fn render(
        &self,
        frame: &mut Frame,
        area: Rect,
        cell: Option<&HistoryCell>,
        spinner_frame: usize,
    ) {
        let popup_area = centered_rect(90, 90, area);
        frame.render_widget(Clear, popup_area);

        let Some(cell) = cell else {
            // Cell not found (e.g., transcript cleared)
            let block = Block::default()
                .title(" Tool Detail ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title_bottom(" [q] close ");
            let inner = block.inner(popup_area);
            frame.render_widget(block, popup_area);
            frame.render_widget(Paragraph::new("Tool not found in transcript."), inner);
            return;
        };

        let HistoryCell::Tool {
            name,
            state,
            input,
            result,
            started_at,
            completed_at,
            input_delta,
            ..
        } = cell
        else {
            return;
        };

        // Build title with icon (animated spinner for running tools)
        let icon = match state {
            ToolState::Running => {
                let idx = (spinner_frame / SPINNER_SPEED_DIVISOR) % SPINNER_FRAMES.len();
                SPINNER_FRAMES[idx]
            }
            ToolState::Done => "✓",
            ToolState::Error => "✗",
            ToolState::Cancelled => "⊘",
        };
        let title = format!(" {icon} {name} ");

        let border_color = match state {
            ToolState::Running => Color::Cyan,
            ToolState::Done => Color::Green,
            ToolState::Error => Color::Red,
            ToolState::Cancelled => Color::Yellow,
        };

        // Build content lines first, then construct block with scroll indicator.
        let mut lines: Vec<Line<'static>> = Vec::new();

        // --- Status section ---
        let status_text = match state {
            ToolState::Running => "Running…".to_string(),
            ToolState::Done => {
                if let Some(completed) = completed_at {
                    let elapsed = completed.signed_duration_since(*started_at);
                    format!("Done ({:.1}s)", elapsed.num_milliseconds() as f64 / 1000.0)
                } else {
                    "Done".to_string()
                }
            }
            ToolState::Error => "Error".to_string(),
            ToolState::Cancelled => "Cancelled".to_string(),
        };
        lines.push(Line::from(vec![
            Span::styled(
                "Status: ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(status_text, Style::default().fg(border_color)),
        ]));
        lines.push(Line::from(""));

        // --- Args section ---
        lines.push(Line::from(Span::styled(
            "─── Args ───",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        let pretty_args = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
        for line in pretty_args.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.push(Line::from(""));

        // --- Output section ---
        lines.push(Line::from(Span::styled(
            "─── Output ───",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));

        if let Some(res) = result {
            if let Some(data) = res.data() {
                let output_text = build_popup_output_text(name, data);
                for line in output_text.lines() {
                    lines.push(Line::from(Span::styled(
                        line.to_string(),
                        Style::default().fg(Color::White),
                    )));
                }

                // Truncation warnings
                if data
                    .get("stdout_truncated")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    let total = data
                        .get("stdout_total_bytes")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    let warning = format_byte_truncation("stdout", total);
                    lines.push(Line::from(Span::styled(
                        format!("⚠ {warning}"),
                        Style::default().fg(Color::Yellow),
                    )));
                }
                if data
                    .get("stderr_truncated")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    let total = data
                        .get("stderr_total_bytes")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    let warning = format_byte_truncation("stderr", total);
                    lines.push(Line::from(Span::styled(
                        format!("⚠ {warning}"),
                        Style::default().fg(Color::Yellow),
                    )));
                }
                if data
                    .get("truncated")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    let total_lines_val = data
                        .get("total_lines")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    let lines_shown = data
                        .get("lines_shown")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    lines.push(Line::from(Span::styled(
                        format!(
                            "⚠ file truncated: showing {lines_shown} of {total_lines_val} lines"
                        ),
                        Style::default().fg(Color::Yellow),
                    )));
                }
            }

            // Error info
            if let Some((code, message, details)) = res.error_info() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("Error [{code}]: {message}"),
                    Style::default().fg(Color::Red),
                )));
                if let Some(detail_text) = details {
                    for detail_line in detail_text.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {detail_line}"),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                }
            }
        } else if *state == ToolState::Running {
            // Show streaming input_delta if available
            if let Some(delta) = input_delta {
                for line in delta.lines() {
                    lines.push(Line::from(Span::styled(
                        line.to_string(),
                        Style::default().fg(Color::Cyan),
                    )));
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "Waiting for output…",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        } else {
            lines.push(Line::from(Span::styled(
                "(no output)",
                Style::default().fg(Color::DarkGray),
            )));
        }

        // Compute scroll info using a temporary block to get inner height.
        let tmp_block = Block::default().borders(Borders::ALL);
        let inner = tmp_block.inner(popup_area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let viewport_height = inner.height as usize;

        // Compute wrapped line count for correct scroll bounds.
        // Paragraph::scroll with Wrap scrolls by visual (wrapped) lines,
        // so we need the wrapped total, not the unwrapped line count.
        let wrapped_total: usize = lines
            .iter()
            .map(|line| {
                let content_width = line.width();
                if content_width == 0 {
                    1
                } else {
                    content_width.div_ceil(inner.width as usize).max(1)
                }
            })
            .sum();
        let max_scroll = wrapped_total.saturating_sub(viewport_height);

        // Auto-scroll: if the tool is running and user hasn't manually scrolled,
        // keep the view pinned to the bottom.
        if *state == ToolState::Running && !self.user_scrolled.get() {
            self.scroll_offset.set(max_scroll);
        }

        // Clamp stored offset so it never stays inflated past max_scroll.
        // Without this, scrolling back up after hitting the bottom feels stuck
        // because the offset must decrement back through the overshoot first.
        let clamped = self.scroll_offset.get().min(max_scroll);
        self.scroll_offset.set(clamped);
        let effective_scroll = clamped;

        // Build block with scroll position indicator in bottom border.
        let scroll_indicator = if wrapped_total > viewport_height {
            let current_line = effective_scroll + 1;
            format!(" [{current_line}/{wrapped_total}] ")
        } else {
            String::new()
        };

        let block = Block::default()
            .title(title)
            .title_style(
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title_bottom(Line::from(vec![
                Span::styled(" [q]", Style::default().fg(Color::Yellow)),
                Span::styled(" close  ", Style::default().fg(Color::DarkGray)),
                Span::styled("[j/k]", Style::default().fg(Color::Yellow)),
                Span::styled(" scroll ", Style::default().fg(Color::DarkGray)),
                Span::styled(scroll_indicator, Style::default().fg(Color::Cyan)),
            ]));

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let para = Paragraph::new(lines)
            .scroll((effective_scroll.min(u16::MAX as usize) as u16, 0))
            .wrap(Wrap { trim: false });
        frame.render_widget(para, inner);
    }
}
