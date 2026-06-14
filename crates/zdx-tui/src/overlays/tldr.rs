//! Thread TLDR/recap overlay.
//!
//! Shows a quick summary of the user's most recent activity in the current
//! thread. Generated on-demand via a cheap LLM subagent (`Config::tldr_model`)
//! and rendered as scrollable plain markdown.
//!
//! States:
//! - `Loading`: spinner shown while the subagent runs
//! - `Ready(text)`: scrollable summary
//! - `Error(message)`: failure message (e.g. timeout, empty thread)

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::OverlayUpdate;
use super::render_utils::centered_rect;
use crate::transcript::markdown::render_markdown;
use crate::transcript::{SPINNER_SPEED_DIVISOR, convert_styled_line};

/// Spinner frames shared with other overlays.
const SPINNER_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

/// State of the TLDR generation request.
#[derive(Debug, Clone)]
pub enum TldrPhase {
    Loading,
    Ready(String),
    Error(String),
}

#[derive(Debug)]
pub struct TldrState {
    /// Thread the TLDR was requested for. Used to ignore stale results that
    /// arrive after the user switched threads.
    pub thread_id: String,
    pub phase: TldrPhase,
    scroll_offset: Cell<usize>,
}

impl TldrState {
    pub fn open(thread_id: String) -> Self {
        Self {
            thread_id,
            phase: TldrPhase::Loading,
            scroll_offset: Cell::new(0),
        }
    }

    pub fn set_ready(&mut self, text: String) {
        self.phase = TldrPhase::Ready(text);
        self.scroll_offset.set(0);
    }

    pub fn set_error(&mut self, message: String) {
        self.phase = TldrPhase::Error(message);
        self.scroll_offset.set(0);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> OverlayUpdate {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => OverlayUpdate::close(),
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_offset
                    .set(self.scroll_offset.get().saturating_add(1));
                OverlayUpdate::stay()
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_offset
                    .set(self.scroll_offset.get().saturating_sub(1));
                OverlayUpdate::stay()
            }
            KeyCode::PageDown => {
                self.scroll_offset
                    .set(self.scroll_offset.get().saturating_add(10));
                OverlayUpdate::stay()
            }
            KeyCode::PageUp => {
                self.scroll_offset
                    .set(self.scroll_offset.get().saturating_sub(10));
                OverlayUpdate::stay()
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.scroll_offset.set(0);
                OverlayUpdate::stay()
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.scroll_offset.set(usize::MAX);
                OverlayUpdate::stay()
            }
            _ => OverlayUpdate::stay(),
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, _input_y: u16, spinner_frame: usize) {
        let popup_area = centered_rect(80, 70, area);
        frame.render_widget(Clear, popup_area);

        let (icon, border_color, status) = match &self.phase {
            TldrPhase::Loading => {
                let idx = (spinner_frame / SPINNER_SPEED_DIVISOR) % SPINNER_FRAMES.len();
                (SPINNER_FRAMES[idx], Color::Cyan, "Generating…")
            }
            TldrPhase::Ready(_) => ("✓", Color::Green, "TLDR"),
            TldrPhase::Error(_) => ("✗", Color::Red, "Error"),
        };
        let title = format!(" {icon} {status} ");

        // Compute the inner area first so markdown rendering can pre-wrap
        // styled lines to the actual viewport width.
        let tmp_block = Block::default().borders(Borders::ALL);
        let inner = tmp_block.inner(popup_area);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let body_width = inner.width as usize;

        let mut lines: Vec<Line<'static>> = Vec::new();
        match &self.phase {
            TldrPhase::Loading => {
                lines.push(Line::from(Span::styled(
                    "Summarizing recent activity…",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
            TldrPhase::Ready(text) => {
                let styled = render_markdown(text, body_width);
                lines.extend(styled.iter().map(convert_styled_line));
                if lines.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "(empty TLDR)",
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            TldrPhase::Error(message) => {
                lines.push(Line::from(Span::styled(
                    "Could not generate TLDR.".to_string(),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
                for raw in message.lines() {
                    lines.push(Line::from(Span::styled(
                        raw.to_string(),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
        }

        let viewport_height = inner.height as usize;
        let wrapped_total: usize = lines
            .iter()
            .map(|line| {
                let content_width = line.width();
                if content_width == 0 {
                    1
                } else {
                    content_width.div_ceil(body_width.max(1)).max(1)
                }
            })
            .sum();
        let max_scroll = wrapped_total.saturating_sub(viewport_height);
        let clamped = self.scroll_offset.get().min(max_scroll);
        self.scroll_offset.set(clamped);

        let scroll_indicator = if wrapped_total > viewport_height {
            let current_line = clamped + 1;
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
                Span::styled(" [Esc/q]", Style::default().fg(Color::Yellow)),
                Span::styled(" close  ", Style::default().fg(Color::DarkGray)),
                Span::styled("[j/k]", Style::default().fg(Color::Yellow)),
                Span::styled(" scroll  ", Style::default().fg(Color::DarkGray)),
                Span::styled("[g/G]", Style::default().fg(Color::Yellow)),
                Span::styled(" top/bottom ", Style::default().fg(Color::DarkGray)),
                Span::styled(scroll_indicator, Style::default().fg(Color::Cyan)),
            ]));

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let para = Paragraph::new(lines)
            .scroll((clamped.min(u16::MAX as usize) as u16, 0))
            .wrap(Wrap { trim: false });
        frame.render_widget(para, inner);
    }
}
