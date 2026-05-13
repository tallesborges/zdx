//! Context analysis overlay.
//!
//! Shows a per-section breakdown of the current LLM context (system
//! prompt, built-in tools, AGENTS.md files per path, messages, total
//! used / free) for the active thread.
//!
//! Two-phase UX:
//! - On open, the overlay runs an instant local pass that counts raw
//!   characters and renders the breakdown immediately. This is 100%
//!   accurate as a *char* count, just not directly comparable to the
//!   model's token-based context window.
//! - If the active model supports it (Claude family + Anthropic API key
//!   configured), pressing `r` re-runs the analysis against Anthropic's
//!   `/v1/messages/count_tokens` endpoint to get exact per-section token
//!   numbers. After tokens are fetched, the overlay can toggle between
//!   Chars and Tokens views locally with `c` and `r`/`t` — no extra
//!   network call.
//!
//! Closely mirrors the TLDR overlay (`tldr.rs`) for layout and scroll
//! handling. Triggered by `/context` (aliases `ctx`, `context-analyze`)
//! from the input or the command palette.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::OverlayUpdate;
use super::render_utils::centered_rect;
use crate::effects::UiEffect;
use crate::runtime::{AnalysisMode, ContextReport, DisplayMode};
use crate::transcript::markdown::render_markdown;
use crate::transcript::{SPINNER_SPEED_DIVISOR, convert_styled_line};

/// Spinner frames shared with the TLDR overlay.
const SPINNER_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

/// State of the context-analysis generation request.
#[derive(Debug, Clone)]
pub enum ContextPhase {
    Loading,
    Ready(ContextReport),
    Error(String),
}

#[derive(Debug)]
pub struct ContextState {
    pub phase: ContextPhase,
    /// True when the active model + config allow refining via Anthropic's
    /// `count_tokens` endpoint. Controls visibility of the `[r] refine`
    /// hint and whether the `r` keypress will start a fetch.
    pub refine_available: bool,
    /// Local view toggle. `Chars` is the default and is always
    /// renderable; `Tokens` becomes available after the user refines.
    pub display_mode: DisplayMode,
    /// True while a refine fetch is in flight. The existing report (if
    /// any) stays visible underneath so the user keeps the chars view
    /// instead of seeing an empty spinner.
    pub refining: bool,
    scroll_offset: Cell<usize>,
}

impl ContextState {
    pub fn open(refine_available: bool) -> Self {
        Self {
            phase: ContextPhase::Loading,
            refine_available,
            display_mode: DisplayMode::Chars,
            refining: false,
            scroll_offset: Cell::new(0),
        }
    }

    pub fn set_ready(&mut self, report: ContextReport) {
        // If the new report carries tokens (refine just completed),
        // automatically surface the Tokens view — that's what the user
        // asked for by pressing `r`. Otherwise keep whatever mode they're on.
        if report.has_tokens() {
            self.display_mode = DisplayMode::Tokens;
        }
        self.phase = ContextPhase::Ready(report);
        self.refining = false;
        self.scroll_offset.set(0);
    }

    pub fn set_error(&mut self, message: String) {
        self.phase = ContextPhase::Error(message);
        self.refining = false;
        self.scroll_offset.set(0);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> OverlayUpdate {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => OverlayUpdate::close(),
            // `r` — refine to exact tokens, or switch to the cached
            // Tokens view if we already fetched them.
            KeyCode::Char('r') if self.refine_available => self.trigger_refine_or_toggle(),
            // `t` — switch to Tokens view if cached; otherwise behave
            // like `r` and fetch them.
            KeyCode::Char('t') => {
                if self.has_cached_tokens() {
                    self.display_mode = DisplayMode::Tokens;
                    self.scroll_offset.set(0);
                    OverlayUpdate::stay()
                } else if self.refine_available {
                    self.trigger_refine_or_toggle()
                } else {
                    OverlayUpdate::stay()
                }
            }
            // `c` — switch back to Chars view (instant, no fetch).
            KeyCode::Char('c') => {
                self.display_mode = DisplayMode::Chars;
                self.scroll_offset.set(0);
                OverlayUpdate::stay()
            }
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

    fn has_cached_tokens(&self) -> bool {
        matches!(&self.phase, ContextPhase::Ready(r) if r.has_tokens())
    }

    /// Either flip to the cached Tokens view (instant) or kick off a
    /// fetch via Anthropic's `count_tokens`.
    fn trigger_refine_or_toggle(&mut self) -> OverlayUpdate {
        if self.has_cached_tokens() {
            // Tokens already cached: just flip the view.
            self.display_mode = DisplayMode::Tokens;
            self.scroll_offset.set(0);
            return OverlayUpdate::stay();
        }
        // No cached tokens yet — fetch them. Keep the current Ready
        // report visible underneath the spinner so the chars view
        // doesn't disappear during the round-trip.
        self.refining = true;
        self.scroll_offset.set(0);
        OverlayUpdate::stay().with_ui_effects(vec![UiEffect::AnalyzeContext {
            mode: AnalysisMode::Exact,
        }])
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, _input_y: u16, spinner_frame: usize) {
        let popup_area = centered_rect(80, 70, area);
        frame.render_widget(Clear, popup_area);

        let (icon, border_color, status) = match &self.phase {
            ContextPhase::Loading => {
                let idx = (spinner_frame / SPINNER_SPEED_DIVISOR) % SPINNER_FRAMES.len();
                (SPINNER_FRAMES[idx], Color::Cyan, "Analyzing…")
            }
            ContextPhase::Ready(_) if self.refining => {
                let idx = (spinner_frame / SPINNER_SPEED_DIVISOR) % SPINNER_FRAMES.len();
                (SPINNER_FRAMES[idx], Color::Cyan, "Refining…")
            }
            ContextPhase::Ready(_) => {
                let label = match self.display_mode {
                    DisplayMode::Chars => "Context Usage · Chars",
                    DisplayMode::Tokens => "Context Usage · Tokens",
                };
                ("✓", Color::Green, label)
            }
            ContextPhase::Error(_) => ("✗", Color::Red, "Error"),
        };
        let title = format!(" {icon} {status} ");

        let tmp_block = Block::default().borders(Borders::ALL);
        let inner = tmp_block.inner(popup_area);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let body_width = inner.width as usize;

        let lines = self.build_lines(body_width);

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

        let bottom_spans = self.build_bottom_hint(scroll_indicator);

        let block = Block::default()
            .title(title)
            .title_style(
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title_bottom(Line::from(bottom_spans));

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let para = Paragraph::new(lines)
            .scroll((clamped.min(u16::MAX as usize) as u16, 0))
            .wrap(Wrap { trim: false });
        frame.render_widget(para, inner);
    }

    /// Builds the body lines from the current `phase`. Pre-wraps Markdown
    /// against `body_width` so scroll math is line-accurate.
    fn build_lines(&self, body_width: usize) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        match &self.phase {
            ContextPhase::Loading => {
                lines.push(Line::from(Span::styled(
                    "Counting context size for system prompt, tools, AGENTS.md, and messages…",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
            ContextPhase::Ready(report) => {
                let markdown = report.render_markdown(self.display_mode);
                let styled = render_markdown(&markdown, body_width);
                lines.extend(styled.into_iter().map(convert_styled_line));
                if lines.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "(empty report)",
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            ContextPhase::Error(message) => {
                lines.push(Line::from(Span::styled(
                    "Could not analyze context.".to_string(),
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
        lines
    }

    /// Builds the bottom hint row. Shows mode-specific toggle hints:
    /// - In Chars view with tokens cached → `[t] tokens`
    /// - In Chars view, refine supported, no cache → `[r] refine`
    /// - In Tokens view → `[c] chars`
    fn build_bottom_hint(&self, scroll_indicator: String) -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = vec![
            Span::styled(" [Esc/q]", Style::default().fg(Color::Yellow)),
            Span::styled(" close  ", Style::default().fg(Color::DarkGray)),
            Span::styled("[j/k]", Style::default().fg(Color::Yellow)),
            Span::styled(" scroll  ", Style::default().fg(Color::DarkGray)),
            Span::styled("[g/G]", Style::default().fg(Color::Yellow)),
            Span::styled(" top/bottom  ", Style::default().fg(Color::DarkGray)),
        ];

        // Toggle hints only make sense once a report is visible and we're
        // not in the middle of a refine round-trip.
        if !self.refining && matches!(self.phase, ContextPhase::Ready(_)) {
            match self.display_mode {
                DisplayMode::Chars => {
                    if self.has_cached_tokens() {
                        spans.push(Span::styled("[t]", Style::default().fg(Color::Yellow)));
                        spans.push(Span::styled(
                            " tokens  ",
                            Style::default().fg(Color::DarkGray),
                        ));
                    } else if self.refine_available {
                        spans.push(Span::styled("[r]", Style::default().fg(Color::Yellow)));
                        spans.push(Span::styled(
                            " refine via count_tokens  ",
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }
                DisplayMode::Tokens => {
                    spans.push(Span::styled("[c]", Style::default().fg(Color::Yellow)));
                    spans.push(Span::styled(
                        " chars  ",
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
        }

        spans.push(Span::styled(
            scroll_indicator,
            Style::default().fg(Color::Cyan),
        ));
        spans
    }
}
