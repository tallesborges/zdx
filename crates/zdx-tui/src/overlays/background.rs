//! Background-process overlay: lists the background processes started by the
//! current thread and lets the user stop them. Read-only view over the
//! `zdx_engine::background_activity` registry; kill is issued as a `UiEffect`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use zdx_engine::background_activity;

use super::OverlayUpdate;
use super::render_utils::{
    InputHint, calculate_overlay_area, render_hints, render_overlay_container, render_separator,
};
use crate::common::truncate_with_ellipsis;
use crate::effects::UiEffect;
use crate::state::TuiState;

const OVERLAY_WIDTH: u16 = 78;
const MAX_VISIBLE: usize = 10;

#[derive(Debug, Clone)]
pub struct BgEntry {
    pub bg_id: String,
    pub pid: u32,
    pub command: String,
    pub uptime: String,
}

#[derive(Debug, Clone)]
pub struct BackgroundState {
    thread_id: Option<String>,
    pub entries: Vec<BgEntry>,
    pub selected: usize,
    pub offset: usize,
}

impl BackgroundState {
    #[must_use]
    pub fn open(thread_id: Option<String>) -> Self {
        let entries = load_entries(thread_id.as_deref());
        Self {
            thread_id,
            entries,
            selected: 0,
            offset: 0,
        }
    }

    fn refresh(&mut self) {
        self.entries = load_entries(self.thread_id.as_deref());
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
        self.ensure_visible();
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_background(frame, self, area, input_y);
    }

    pub fn handle_key(&mut self, _tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => OverlayUpdate::close(),
            KeyCode::Char('c') if ctrl => OverlayUpdate::close(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                OverlayUpdate::stay()
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                OverlayUpdate::stay()
            }
            KeyCode::Char('r') => {
                self.refresh();
                OverlayUpdate::stay()
            }
            KeyCode::Char('x' | 'd') | KeyCode::Delete => {
                let Some(entry) = self.entries.get(self.selected).cloned() else {
                    return OverlayUpdate::stay();
                };
                // Optimistic remove; the tick-refreshed count reconciles the
                // registry state once the async kill lands.
                self.entries.remove(self.selected);
                if self.selected >= self.entries.len() {
                    self.selected = self.entries.len().saturating_sub(1);
                }
                self.ensure_visible();
                OverlayUpdate::stay()
                    .with_ui_effects(vec![UiEffect::KillBackgroundProcess { bg_id: entry.bg_id }])
            }
            _ => OverlayUpdate::stay(),
        }
    }

    fn visible_height(&self) -> usize {
        self.entries.len().clamp(1, MAX_VISIBLE)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let max_index = self.entries.len().saturating_sub(1) as isize;
        let next = (self.selected as isize + delta).clamp(0, max_index);
        self.selected = next as usize;
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        if self.entries.is_empty() {
            self.offset = 0;
            return;
        }
        let visible = self.visible_height();
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + visible {
            self.offset = self.selected - visible + 1;
        }
    }
}

fn load_entries(thread_id: Option<&str>) -> Vec<BgEntry> {
    background_activity::list_background()
        .into_iter()
        .filter(|p| p.is_running() && p.thread_id.as_deref() == thread_id)
        .map(|p| {
            let uptime = p.uptime();
            BgEntry {
                bg_id: p.bg_id,
                pid: p.pid,
                command: p.command,
                uptime,
            }
        })
        .collect()
}

#[allow(clippy::cast_possible_truncation)]
fn render_background(frame: &mut Frame, state: &BackgroundState, area: Rect, input_y: u16) {
    let visible_rows = state.entries.len().clamp(1, MAX_VISIBLE) as u16;
    let overlay_height = (visible_rows + 5).max(7);
    let overlay_area = calculate_overlay_area(area, input_y, OVERLAY_WIDTH, overlay_height);

    render_overlay_container(frame, overlay_area, "Background processes", Color::Green);

    let inner_area = Rect::new(
        overlay_area.x + 1,
        overlay_area.y + 1,
        overlay_area.width.saturating_sub(2),
        overlay_area.height.saturating_sub(2),
    );

    if state.entries.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(Span::styled(
                "No background processes for this thread",
                Style::default().fg(Color::DarkGray),
            )),
            Line::default(),
            Line::from(Span::styled(
                "Esc to close",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(msg, inner_area);
        return;
    }

    let list_height = inner_area.height.saturating_sub(2) as usize;
    let list_area = Rect::new(
        inner_area.x,
        inner_area.y,
        inner_area.width,
        list_height as u16,
    );

    let max_content_width = inner_area.width.saturating_sub(4).max(1) as usize;
    let mut items = Vec::new();
    for entry in state.entries.iter().skip(state.offset).take(list_height) {
        let meta = format!("pid {:<7} {:>6}  ", entry.pid, entry.uptime);
        let cmd_width = max_content_width
            .saturating_sub(meta.chars().count())
            .max(1);
        let command = truncate_with_ellipsis(&entry.command, cmd_width);
        let line = Line::from(vec![
            Span::styled(meta, Style::default().fg(Color::DarkGray)),
            Span::styled(command, Style::default().fg(Color::White)),
        ]);
        items.push(ListItem::new(line));
    }

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected.saturating_sub(state.offset)));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, inner_area, list_height as u16);

    render_hints(
        frame,
        inner_area,
        &[
            InputHint::new("↑↓", "navigate"),
            InputHint::new("x", "kill"),
            InputHint::new("r", "refresh"),
            InputHint::new("Esc", "close"),
        ],
        Color::Green,
    );
}
