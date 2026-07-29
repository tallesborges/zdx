//! Tool detail popup overlay.
//!
//! Displays full tool information in a near-full-screen popup:
//! args (pretty JSON), output, status, and error details.
//! Opens on click from compact tool header in transcript.
//! Supports live updates for running tools via render-time cell lookup.
//!
//! The body content itself is built by `zdx_transcript::tool_detail_body` so
//! the monitor's tool detail pane renders identical text; this module owns the
//! popup chrome, scrolling, selection, and clipboard.

use std::cell::Cell;
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
use crate::common::grapheme_col_at_width;
use crate::transcript::{
    HistoryCell, LineMapping, PositionMap, SPINNER_SPEED_DIVISOR, SelectionState, ToolState,
    VisualPosition, tool_command_text, tool_detail_body, tool_state_color, tool_state_glyph,
    wrap_line_to_width,
};

/// How long the "✓ copied" flash stays visible after a keyboard copy.
const COPIED_FLASH_WINDOW: Duration = Duration::from_millis(1200);

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
                let body = tool_detail_body(cell);
                plain_text(&body.lines[body.output_start.min(body.lines.len())..])
            }
            CopySection::Full => plain_text(&tool_detail_body(cell).lines),
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
        let icon = tool_state_glyph(state, spinner_frame / SPINNER_SPEED_DIVISOR);
        let title = format!(" {icon} {name} ");
        let border_color = tool_state_color(state);

        // Build body lines (shared with keyboard copy on `y`/`Y`).
        let lines = tool_detail_body(cell).lines;

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
        let visual: Vec<Line<'static>> = lines
            .iter()
            .flat_map(|line| wrap_line_to_width(line, width))
            .collect();

        // Rebuild the position map from visual-line texts for selection copy.
        self.position_map.clear();
        for row in &visual {
            self.position_map
                .push(LineMapping::new(line_text(row), None));
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
            .map(|(idx, row)| {
                let grapheme_count = line_text(&row).graphemes(true).count();
                let sel = self.selection.line_selection(idx, grapheme_count);
                Line::from(highlight_spans(&row.spans, sel))
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

/// Plain text of a single styled line.
fn line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Joins the plain text of styled lines with newlines (for clipboard copy).
fn plain_text(lines: &[Line<'static>]) -> String {
    lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
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
