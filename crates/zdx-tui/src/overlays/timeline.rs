use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use serde_json::json;
use zdx_core::core::thread_persistence::ThreadEvent;

use super::OverlayUpdate;
use crate::common::{TaskKind, sanitize_for_display, truncate_with_ellipsis};
use crate::effects::UiEffect;
use crate::mutations::{StateMutation, TranscriptMutation};
use crate::state::TuiState;
use crate::transcript::{HistoryCell, ScrollMode, ScrollState};

const MAX_VISIBLE_TURNS: usize = 12;
const OVERLAY_WIDTH: u16 = 70;

#[derive(Debug, Clone, Copy)]
pub enum TimelineRole {
    User,
    Assistant,
}

impl TimelineRole {
    fn badge(self) -> &'static str {
        match self {
            TimelineRole::User => "U",
            TimelineRole::Assistant => "A",
        }
    }

    fn color(self) -> Color {
        match self {
            TimelineRole::User => Color::Cyan,
            TimelineRole::Assistant => Color::Magenta,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub cell_index: usize,
    pub role: TimelineRole,
    pub preview: String,
}

#[derive(Debug, Clone)]
pub struct TimelineState {
    pub entries: Vec<TimelineEntry>,
    pub selected: usize,
    pub offset: usize,
    initial_scroll: ScrollMode,
}

impl TimelineState {
    pub fn open(
        cells: &[HistoryCell],
        scroll: &ScrollState,
        initial_scroll: ScrollMode,
    ) -> (Self, Vec<UiEffect>, Vec<StateMutation>) {
        let entries = build_entries(cells);
        let initial_offset = entries
            .first()
            .and_then(|entry| scroll.cell_start_line(entry.cell_index));
        let mut mutations = Vec::new();
        if let Some(offset) = initial_offset {
            mutations.push(StateMutation::Transcript(
                TranscriptMutation::SetScrollOffset { offset },
            ));
        }
        (
            Self {
                entries,
                selected: 0,
                offset: 0,
                initial_scroll,
            },
            vec![],
            mutations,
        )
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_timeline(frame, self, area, input_y)
    }

    pub fn handle_key(&mut self, tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                OverlayUpdate::close().with_mutations(vec![StateMutation::Transcript(
                    TranscriptMutation::SetScrollMode(self.initial_scroll.clone()),
                )])
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                OverlayUpdate::stay().with_mutations(self.preview_scroll_command(tui))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                OverlayUpdate::stay().with_mutations(self.preview_scroll_command(tui))
            }
            KeyCode::PageUp => {
                let delta = -(self.visible_height() as isize).max(1);
                self.move_selection(delta);
                OverlayUpdate::stay().with_mutations(self.preview_scroll_command(tui))
            }
            KeyCode::PageDown => {
                let delta = (self.visible_height() as isize).max(1);
                self.move_selection(delta);
                OverlayUpdate::stay().with_mutations(self.preview_scroll_command(tui))
            }
            KeyCode::Home => {
                if !self.entries.is_empty() {
                    self.selected = 0;
                    self.offset = 0;
                }
                OverlayUpdate::stay().with_mutations(self.preview_scroll_command(tui))
            }
            KeyCode::End => {
                if !self.entries.is_empty() {
                    self.selected = self.entries.len().saturating_sub(1);
                    self.ensure_visible();
                }
                OverlayUpdate::stay().with_mutations(self.preview_scroll_command(tui))
            }
            KeyCode::Enter | KeyCode::Right => {
                if tui.agent_state.is_running() {
                    return OverlayUpdate::stay().with_mutations(vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "Stop the current task first.".to_string(),
                        ),
                    )]);
                }

                match self.jump_command(tui) {
                    Some(command) => OverlayUpdate::close()
                        .with_mutations(vec![StateMutation::Transcript(command)]),
                    None => OverlayUpdate::stay().with_mutations(vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "No timeline entry selected.".to_string(),
                        ),
                    )]),
                }
            }
            KeyCode::Char('f') => {
                if tui.agent_state.is_running() {
                    return OverlayUpdate::stay().with_mutations(vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "Stop the current task first.".to_string(),
                        ),
                    )]);
                }

                if tui.tasks.state(TaskKind::ThreadFork).is_running() {
                    return OverlayUpdate::stay();
                }

                match self.fork_effect(tui) {
                    Some(effect) => OverlayUpdate::close()
                        .with_ui_effects(vec![effect])
                        .with_mutations(vec![]),
                    None => OverlayUpdate::stay().with_mutations(vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "No timeline entry selected.".to_string(),
                        ),
                    )]),
                }
            }
            _ => OverlayUpdate::stay(),
        }
    }

    fn visible_height(&self) -> usize {
        if self.entries.is_empty() {
            1
        } else {
            self.entries.len().min(MAX_VISIBLE_TURNS)
        }
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

        let visible_height = self.visible_height();
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + visible_height {
            self.offset = self.selected - visible_height + 1;
        }
    }

    fn selected_entry(&self) -> Option<&TimelineEntry> {
        self.entries.get(self.selected)
    }

    fn preview_scroll_command(&self, tui: &TuiState) -> Vec<StateMutation> {
        self.jump_command(tui)
            .map(|command| vec![StateMutation::Transcript(command)])
            .unwrap_or_default()
    }

    fn jump_command(&self, tui: &TuiState) -> Option<TranscriptMutation> {
        let entry = self.selected_entry()?;
        let info = tui.transcript.scroll.cell_line_info.get(entry.cell_index)?;
        Some(TranscriptMutation::SetScrollOffset {
            offset: info.start_line,
        })
    }

    fn fork_effect(&self, tui: &TuiState) -> Option<UiEffect> {
        let entry = self.selected_entry()?;
        let cells = tui.transcript.cells();
        let selected_cell = cells.get(entry.cell_index)?;
        let (events, user_input) = match selected_cell {
            HistoryCell::User { content, .. } => (
                cells_to_events(&cells[..entry.cell_index]),
                Some(content.clone()),
            ),
            _ => (cells_to_events(&cells[..=entry.cell_index]), None),
        };
        if events.is_empty() && user_input.is_none() {
            return None;
        }

        Some(UiEffect::ForkThread {
            events,
            user_input,
            turn_number: self.selected + 1,
        })
    }
}

fn build_entries(cells: &[HistoryCell]) -> Vec<TimelineEntry> {
    cells
        .iter()
        .enumerate()
        .filter_map(|(idx, cell)| match cell {
            HistoryCell::User { content, .. } => Some((idx, TimelineRole::User, content)),
            HistoryCell::Assistant { content, .. } => Some((idx, TimelineRole::Assistant, content)),
            _ => None,
        })
        .map(|(idx, role, content)| {
            let sanitized = sanitize_for_display(content);
            let line = sanitized.lines().next().unwrap_or("").trim();
            TimelineEntry {
                cell_index: idx,
                role,
                preview: line.to_string(),
            }
        })
        .collect()
}

fn cells_to_events(cells: &[HistoryCell]) -> Vec<ThreadEvent> {
    let mut events = Vec::new();

    for cell in cells {
        match cell {
            HistoryCell::User { content, .. } => {
                events.push(ThreadEvent::user_message(content));
            }
            HistoryCell::Assistant { content, .. } => {
                events.push(ThreadEvent::assistant_message(content));
            }
            HistoryCell::Thinking {
                content, replay, ..
            } => {
                events.push(ThreadEvent::reasoning(
                    Some(content.clone()),
                    replay.clone(),
                ));
            }
            HistoryCell::Tool {
                tool_use_id,
                name,
                input,
                result,
                ..
            } => {
                events.push(ThreadEvent::tool_use(
                    tool_use_id.clone(),
                    name.clone(),
                    input.clone(),
                ));
                if let Some(output) = result {
                    let value = serde_json::to_value(output)
                        .unwrap_or_else(|_| json!({"ok": false, "error": "serialize_failed"}));
                    events.push(ThreadEvent::tool_result(
                        tool_use_id.clone(),
                        value,
                        output.is_ok(),
                    ));
                }
            }
            HistoryCell::System { .. } => {}
            HistoryCell::Timing { .. } => {}
        }
    }

    events
}

fn render_timeline(frame: &mut Frame, state: &TimelineState, area: Rect, input_y: u16) {
    use super::render_utils::{
        InputHint, calculate_overlay_area, render_hints, render_overlay_container, render_separator,
    };

    let visible_rows = state.entries.len().clamp(1, MAX_VISIBLE_TURNS) as u16;
    let overlay_height = (visible_rows + 5).max(7);
    let overlay_area = calculate_overlay_area(area, input_y, OVERLAY_WIDTH, overlay_height);

    render_overlay_container(frame, overlay_area, "Timeline", Color::Green);

    let inner_area = Rect::new(
        overlay_area.x + 1,
        overlay_area.y + 1,
        overlay_area.width.saturating_sub(2),
        overlay_area.height.saturating_sub(2),
    );

    if state.entries.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(Span::styled(
                "No turns yet",
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

    let mut items = Vec::new();
    let max_content_width = inner_area.width.saturating_sub(6).max(1) as usize;

    for entry in state.entries.iter().skip(state.offset).take(list_height) {
        let role_label = format!("[{}] ", entry.role.badge());
        let preview = truncate_with_ellipsis(&entry.preview, max_content_width);
        let line = Line::from(vec![
            Span::styled(role_label, Style::default().fg(entry.role.color())),
            Span::styled(preview, Style::default().fg(Color::White)),
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
    let visible_selected = state.selected.saturating_sub(state.offset);
    list_state.select(Some(visible_selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, inner_area, list_height as u16);

    render_hints(
        frame,
        inner_area,
        &[
            InputHint::new("↑↓", "navigate"),
            InputHint::new("Enter", "jump"),
            InputHint::new("f", "fork"),
            InputHint::new("Esc", "close"),
        ],
        Color::Green,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_entries_filters_and_trims() {
        let cells = vec![
            HistoryCell::system("skip"),
            HistoryCell::user("Hello\nSecond"),
            HistoryCell::assistant("Reply"),
        ];
        let entries = build_entries(&cells);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].preview, "Hello");
        assert_eq!(entries[1].preview, "Reply");
    }
}
