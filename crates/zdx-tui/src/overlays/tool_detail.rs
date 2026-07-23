//! Tool detail popup overlay.
//!
//! Displays full tool information in a near-full-screen popup:
//! args (pretty JSON), output, status, and error details.
//! Opens on click from compact tool header in transcript.
//! Supports live updates for running tools via render-time cell lookup.

use std::cell::Cell;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;

use super::OverlayUpdate;
use super::render_utils::centered_rect;
use crate::common::clipboard::Clipboard;
use crate::common::{grapheme_col_at_width, ratatui_width};
use crate::transcript::{
    ChildToolState, HistoryCell, LineMapping, PositionMap, SPINNER_SPEED_DIVISOR, SelectionState,
    ToolState, VisualPosition, tool_command_text,
};

/// Spinner frames for popup title animation.
const SPINNER_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

/// How long the "✓ copied" flash stays visible after a keyboard copy.
const COPIED_FLASH_WINDOW: Duration = Duration::from_millis(1200);

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

/// Which part of a tool cell a keyboard copy key targets.
#[derive(Debug, Clone, Copy)]
enum CopySection {
    /// Output section body only (`y`).
    Output,
    /// The entire popup body (`Y`).
    Full,
    /// The raw primary command/target (`c`).
    Command,
}

#[derive(Debug)]
pub struct ToolDetailState {
    pub tool_use_id: String,
    scroll_offset: Cell<usize>,
    /// True when user has manually scrolled; disables auto-scroll.
    user_scrolled: Cell<bool>,
    /// Mouse text selection over the popup content.
    selection: SelectionState,
    /// Visual-line → text map, rebuilt each render for selection extraction.
    position_map: PositionMap,
    /// Inner content rect from the last render (for screen→text mapping).
    content_area: Cell<Rect>,
    /// When a keyboard copy last happened (drives the "✓ copied" flash).
    copied_flash: Cell<Option<Instant>>,
}

impl ToolDetailState {
    pub fn open(tool_use_id: String) -> Self {
        Self {
            tool_use_id,
            scroll_offset: Cell::new(0),
            user_scrolled: Cell::new(false),
            selection: SelectionState::new(),
            position_map: PositionMap::new(),
            content_area: Cell::new(Rect::default()),
            copied_flash: Cell::new(None),
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

    /// Starts a selection at the given screen coordinates (mouse press).
    pub fn selection_mouse_down(&mut self, x: u16, y: u16) {
        if let Some((line, col)) = self.screen_to_pos(x, y) {
            self.selection.start(VisualPosition::new(line, col));
        } else {
            self.selection.clear();
        }
    }

    /// Extends the active selection to the given screen coordinates (mouse drag).
    pub fn selection_mouse_drag(&mut self, x: u16, y: u16) {
        if self.selection.is_selecting
            && let Some((line, col)) = self.screen_to_pos(x, y)
        {
            self.selection.extend(VisualPosition::new(line, col));
        }
    }

    /// Finishes selecting (mouse release) and auto-copies any selected text.
    pub fn selection_mouse_up(&mut self) {
        self.selection.finish();
        if self.selection.has_selection() {
            self.copy_selection();
        }
    }

    /// Returns true when a copy-feedback clear is pending (drives animation ticks).
    pub fn has_pending_selection_clear(&self) -> bool {
        self.selection.has_pending_clear()
    }

    /// Clears the selection once the copy-feedback delay elapses.
    ///
    /// Returns true if the selection was cleared.
    pub fn check_selection_timeout(&mut self) -> bool {
        self.selection.check_and_clear()
    }

    /// Copies the current selection to the clipboard and schedules a visual clear.
    fn copy_selection(&mut self) {
        let Some((start, end)) = self.selection.get_range() else {
            return;
        };
        let text = self.position_map.get_text_range(start, end);
        if text.is_empty() {
            return;
        }
        if Clipboard::copy(&text).is_ok() {
            self.selection.schedule_clear();
        }
    }

    /// Maps screen coordinates to a `(visual_line, grapheme_column)` position.
    ///
    /// Uses the inner content rect and effective scroll from the last render.
    fn screen_to_pos(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        let area = self.content_area.get();
        if x < area.x
            || x >= area.x.saturating_add(area.width)
            || y < area.y
            || y >= area.y.saturating_add(area.height)
        {
            return None;
        }
        let content_x = (x - area.x) as usize;
        let content_y = (y - area.y) as usize;
        let visual_line = self.scroll_offset.get() + content_y;
        let mapping = self.position_map.get(visual_line)?;
        Some((visual_line, grapheme_col_at_width(&mapping.text, content_x)))
    }

    /// Copies part of the tool cell to the clipboard and flashes "✓ copied".
    ///
    /// Computed on demand from the live cell so no text is cached between renders.
    fn copy_section(&self, cell: Option<&HistoryCell>, section: CopySection) {
        let Some(cell) = cell else {
            return;
        };
        let text = match section {
            CopySection::Command => match cell {
                HistoryCell::Tool { name, input, .. } => tool_command_text(name, input),
                _ => String::new(),
            },
            CopySection::Output => {
                let (lines, output_start) = build_content_lines(cell);
                plain_text(&lines[output_start.min(lines.len())..])
            }
            CopySection::Full => plain_text(&build_content_lines(cell).0),
        };
        if !text.is_empty() && Clipboard::copy(&text).is_ok() {
            self.copied_flash.set(Some(Instant::now()));
        }
    }

    /// Returns true while the "✓ copied" flash should remain visible.
    pub fn should_show_copied_flash(&self) -> bool {
        self.copied_flash
            .get()
            .is_some_and(|at| at.elapsed() < COPIED_FLASH_WINDOW)
    }

    pub fn handle_key(&mut self, cell: Option<&HistoryCell>, key: KeyEvent) -> OverlayUpdate {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => OverlayUpdate::close(),
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_down(1);
                OverlayUpdate::stay()
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_up(1);
                OverlayUpdate::stay()
            }
            KeyCode::PageDown => {
                self.scroll_down(20);
                OverlayUpdate::stay()
            }
            KeyCode::PageUp => {
                self.scroll_up(20);
                OverlayUpdate::stay()
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.scroll_offset.set(0);
                self.user_scrolled.set(true);
                OverlayUpdate::stay()
            }
            KeyCode::Char('y') => {
                self.copy_section(cell, CopySection::Output);
                OverlayUpdate::stay()
            }
            KeyCode::Char('Y') => {
                self.copy_section(cell, CopySection::Full);
                OverlayUpdate::stay()
            }
            KeyCode::Char('c') => {
                self.copy_section(cell, CopySection::Command);
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

        let HistoryCell::Tool { name, state, .. } = cell else {
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
        let border_color = state_color(state);

        // Build body lines (shared with keyboard copy on `y`/`Y`).
        let (lines, _output_start) = build_content_lines(cell);

        // Compute inner content rect (borders consume one cell per side).
        let inner = Block::default().borders(Borders::ALL).inner(popup_area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let viewport_height = inner.height as usize;
        let width = inner.width as usize;

        // Pre-wrap into visual rows so selection maps 1:1 to rendered lines.
        // We render these directly (no Paragraph::wrap) so screen coordinates
        // line up with the position map used for copy.
        let mut visual: Vec<(Vec<Span<'static>>, String)> = Vec::new();
        for line in &lines {
            visual.extend(wrap_line(line, width));
        }

        // Rebuild the position map from visual-line texts for selection copy.
        self.position_map.clear();
        for (_, text) in &visual {
            self.position_map.push(LineMapping::new(text.clone(), None));
        }
        self.content_area.set(inner);

        let wrapped_total = visual.len();
        let max_scroll = wrapped_total.saturating_sub(viewport_height);

        // Auto-scroll: if the tool is running and user hasn't manually scrolled,
        // keep the view pinned to the bottom.
        if *state == ToolState::Running && !self.user_scrolled.get() {
            self.scroll_offset.set(max_scroll);
        }

        // Clamp stored offset so it never stays inflated past max_scroll.
        let effective_scroll = self.scroll_offset.get().min(max_scroll);
        self.scroll_offset.set(effective_scroll);

        // Apply selection highlight per visual line (reversed background).
        let rendered: Vec<Line<'static>> = visual
            .into_iter()
            .enumerate()
            .map(|(idx, (spans, text))| {
                let grapheme_count = text.graphemes(true).count();
                let sel = self.selection.line_selection(idx, grapheme_count);
                Line::from(highlight_spans(&spans, sel))
            })
            .collect();

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
            .title_bottom(Line::from(footer_spans(
                &scroll_indicator,
                self.should_show_copied_flash(),
            )));

        frame.render_widget(block, popup_area);

        let para =
            Paragraph::new(rendered).scroll((effective_scroll.min(u16::MAX as usize) as u16, 0));
        frame.render_widget(para, inner);
    }
}

/// Border/status color for a tool state.
fn state_color(state: &ToolState) -> Color {
    match state {
        ToolState::Running => Color::Cyan,
        ToolState::Done => Color::Green,
        ToolState::Error => Color::Red,
        ToolState::Cancelled => Color::Yellow,
    }
}

/// Joins the plain text of styled lines with newlines (for clipboard copy).
fn plain_text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Builds the popup body lines (status, args, child tools, output) for a tool.
///
/// Returns the styled lines plus the index of the first output-body line (right
/// after the "─── Output ───" header) so keyboard copy can slice the output
/// section. Shared by `render` and `handle_key` so display and copy never drift.
#[allow(clippy::too_many_lines)]
fn build_content_lines(cell: &HistoryCell) -> (Vec<Line<'static>>, usize) {
    let HistoryCell::Tool {
        name,
        state,
        input,
        result,
        started_at,
        completed_at,
        input_delta,
        output_delta,
        child_tools,
        ..
    } = cell
    else {
        return (Vec::new(), 0);
    };

    let border_color = state_color(state);
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

    // --- Child tools section (relayed subagent activity) ---
    if !child_tools.is_empty() {
        lines.push(Line::from(Span::styled(
            "─── Child tools ───",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        for entry in child_tools {
            let (glyph, color) = match entry.state {
                ChildToolState::Running => ("⟳", Color::Cyan),
                ChildToolState::Done => ("✓", Color::Green),
                ChildToolState::Error => ("✗", Color::Red),
            };
            let mut text = format!("{glyph} {}", entry.name);
            if let Some(arg) = entry.key_arg.as_deref().filter(|arg| !arg.is_empty()) {
                let _ = write!(text, "  {arg}");
            }
            lines.push(Line::from(Span::styled(text, Style::default().fg(color))));
        }
        lines.push(Line::from(""));
    }

    // --- Output section ---
    lines.push(Line::from(Span::styled(
        "─── Output ───",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    let output_start = lines.len();

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
                    format!("⚠ file truncated: showing {lines_shown} of {total_lines_val} lines"),
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
        // Show streaming output_delta first, then input_delta, then placeholder
        if let Some(delta) = output_delta.as_deref().filter(|d| !d.is_empty()) {
            for line in delta.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::White),
                )));
            }
        } else if let Some(delta) = input_delta.as_deref().filter(|d| !d.is_empty()) {
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
    } else if let Some(delta) = output_delta.as_deref().filter(|d| !d.is_empty()) {
        // Show preserved partial output for cancelled/errored tools
        for line in delta.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "(no output)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    (lines, output_start)
}

/// Builds the bottom-border hint spans for the popup.
///
/// Shows a green "✓ copied" flash in place of the drag hint right after a
/// keyboard copy; otherwise shows the drag-to-copy hint.
fn footer_spans(scroll_indicator: &str, copied: bool) -> Vec<Span<'static>> {
    let key = Style::default().fg(Color::Yellow);
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans = vec![
        Span::styled(" [Esc/q]", key),
        Span::styled(" close  ", dim),
        Span::styled("[j/k]", key),
        Span::styled(" scroll  ", dim),
        Span::styled("[g/G]", key),
        Span::styled(" top/bottom  ", dim),
        Span::styled("[y]", key),
        Span::styled(" output  ", dim),
        Span::styled("[Y]", key),
        Span::styled(" all  ", dim),
        Span::styled("[c]", key),
        Span::styled(" cmd  ", dim),
    ];
    if copied {
        spans.push(Span::styled(
            "✓ copied ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled("drag", key));
        spans.push(Span::styled(" copies ", dim));
    }
    spans.push(Span::styled(
        scroll_indicator.to_string(),
        Style::default().fg(Color::Cyan),
    ));
    spans
}

/// Hard-wraps a styled line to `width` display columns, preserving span styles.
///
/// Returns visual rows as `(spans, plain_text)`; the plain text feeds the
/// selection position map so copy matches exactly what is rendered.
fn wrap_line(line: &Line<'static>, width: usize) -> Vec<(Vec<Span<'static>>, String)> {
    if width == 0 {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        return vec![(line.spans.clone(), text)];
    }

    let mut rows: Vec<(Vec<Span<'static>>, String)> = Vec::new();
    let mut row_spans: Vec<Span<'static>> = Vec::new();
    let mut row_text = String::new();
    let mut row_width = 0usize;
    let mut seg_text = String::new();
    let mut seg_style = Style::default();

    for span in &line.spans {
        let style = span.style;
        for grapheme in span.content.graphemes(true) {
            let grapheme_width = ratatui_width(grapheme);
            if row_width + grapheme_width > width && row_width > 0 {
                if !seg_text.is_empty() {
                    row_spans.push(Span::styled(std::mem::take(&mut seg_text), seg_style));
                }
                rows.push((
                    std::mem::take(&mut row_spans),
                    std::mem::take(&mut row_text),
                ));
                row_width = 0;
            }
            if seg_style != style && !seg_text.is_empty() {
                row_spans.push(Span::styled(std::mem::take(&mut seg_text), seg_style));
            }
            seg_style = style;
            seg_text.push_str(grapheme);
            row_text.push_str(grapheme);
            row_width += grapheme_width;
        }
    }

    if !seg_text.is_empty() {
        row_spans.push(Span::styled(seg_text, seg_style));
    }
    rows.push((row_spans, row_text));
    rows
}

/// Rebuilds spans with the selected grapheme range rendered reversed.
fn highlight_spans(
    spans: &[Span<'static>],
    selection: Option<(usize, usize)>,
) -> Vec<Span<'static>> {
    let Some((sel_start, sel_end)) = selection else {
        return spans.to_vec();
    };
    if sel_start >= sel_end {
        return spans.to_vec();
    }

    let mut result: Vec<Span<'static>> = Vec::new();
    let mut current = 0usize;
    for span in spans {
        let graphemes: Vec<&str> = span.content.graphemes(true).collect();
        let len = graphemes.len();
        let span_end = current + len;
        let base = span.style;

        let overlap_start = sel_start.max(current);
        let overlap_end = sel_end.min(span_end);

        if overlap_start >= overlap_end {
            result.push(span.clone());
        } else {
            let rel_start = overlap_start - current;
            let rel_end = overlap_end - current;
            if rel_start > 0 {
                result.push(Span::styled(graphemes[..rel_start].join(""), base));
            }
            result.push(Span::styled(
                graphemes[rel_start..rel_end].join(""),
                base.add_modifier(Modifier::REVERSED),
            ));
            if rel_end < len {
                result.push(Span::styled(graphemes[rel_end..].join(""), base));
            }
        }
        current = span_end;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_texts(rows: &[(Vec<Span<'static>>, String)]) -> Vec<String> {
        rows.iter().map(|(_, t)| t.clone()).collect()
    }

    #[test]
    fn wrap_line_hard_wraps_by_display_width() {
        let line = Line::from("abcdef");
        let rows = wrap_line(&line, 3);
        assert_eq!(row_texts(&rows), vec!["abc", "def"]);
    }

    #[test]
    fn wrap_line_preserves_empty_line() {
        let line = Line::from("");
        let rows = wrap_line(&line, 10);
        assert_eq!(row_texts(&rows), vec![String::new()]);
    }

    #[test]
    fn wrap_line_keeps_full_text_across_rows() {
        // Wide graphemes count as their display width; text must round-trip.
        let line = Line::from("a你b好c");
        let rows = wrap_line(&line, 3);
        let joined: String = rows.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(joined, "a你b好c");
    }

    #[test]
    fn highlight_spans_reverses_only_selected_range() {
        let spans = vec![Span::raw("hello world")];
        // Select graphemes 6..11 ("world").
        let out = highlight_spans(&spans, Some((6, 11)));
        let reversed: String = out
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(reversed, "world");
    }

    #[test]
    fn highlight_spans_no_selection_is_passthrough() {
        let spans = vec![Span::raw("abc")];
        let out = highlight_spans(&spans, None);
        assert_eq!(out.len(), 1);
        assert!(!out[0].style.add_modifier.contains(Modifier::REVERSED));
    }
}
