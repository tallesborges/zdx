use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use super::OverlayAction;
use crate::core::session::{self, SessionSummary, short_session_id};
use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::state::TuiState;
use crate::ui::transcript::HistoryCell;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MAX_VISIBLE_SESSIONS: usize = 10;
const VISIBLE_HEIGHT: usize = MAX_VISIBLE_SESSIONS - 2;

#[derive(Debug, Clone)]
pub struct SessionPickerState {
    pub sessions: Vec<SessionSummary>,
    pub selected: usize,
    pub offset: usize,
    pub original_cells: Vec<HistoryCell>,
}

impl SessionPickerState {
    pub fn open(
        sessions: Vec<SessionSummary>,
        original_cells: Vec<HistoryCell>,
    ) -> (Self, Vec<UiEffect>) {
        let state = Self {
            sessions,
            selected: 0,
            offset: 0,
            original_cells,
        };
        let effects = state
            .selected_session()
            .map(|session| {
                vec![UiEffect::PreviewSession {
                    session_id: session.id.clone(),
                }]
            })
            .unwrap_or_default();
        (state, effects)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_session_picker(frame, self, area, input_y)
    }

    pub fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                tui.transcript.cells = self.original_cells.clone();
                tui.transcript.scroll.reset();
                tui.transcript.wrap_cache.clear();
                Some(OverlayAction::close())
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                    if self.selected < self.offset {
                        self.offset = self.selected;
                    }
                }
                self.selected_session().map(|session| {
                    OverlayAction::Effects(vec![UiEffect::PreviewSession {
                        session_id: session.id.clone(),
                    }])
                })
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected < self.sessions.len().saturating_sub(1) {
                    self.selected += 1;
                    if self.selected >= self.offset + VISIBLE_HEIGHT {
                        self.offset = self.selected - VISIBLE_HEIGHT + 1;
                    }
                }
                self.selected_session().map(|session| {
                    OverlayAction::Effects(vec![UiEffect::PreviewSession {
                        session_id: session.id.clone(),
                    }])
                })
            }
            KeyCode::Enter => {
                if tui.agent_state.is_running() {
                    tui.transcript
                        .cells
                        .push(HistoryCell::system("Stop the current task first."));
                    return None;
                }

                if let Some(session) = self.selected_session() {
                    Some(OverlayAction::close_with(vec![UiEffect::LoadSession {
                        session_id: session.id.clone(),
                    }]))
                } else {
                    Some(OverlayAction::close())
                }
            }
            _ => None,
        }
    }

    pub fn selected_session(&self) -> Option<&SessionSummary> {
        self.sessions.get(self.selected)
    }
}

pub fn render_session_picker(
    frame: &mut Frame,
    picker: &SessionPickerState,
    area: Rect,
    input_top_y: u16,
) {
    let session_count = picker.sessions.len();
    let visible_count = session_count.min(MAX_VISIBLE_SESSIONS);

    let picker_width = 60.min(area.width.saturating_sub(4));
    let picker_height = (visible_count as u16 + 5).min(area.height / 2);

    let available_height = input_top_y;

    let picker_x = (area.width.saturating_sub(picker_width)) / 2;
    let picker_y = (available_height.saturating_sub(picker_height)) / 2;

    let picker_area = Rect::new(picker_x, picker_y, picker_width, picker_height);

    frame.render_widget(Clear, picker_area);

    let title = format!(" Sessions ({}) ", session_count);
    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(title)
        .title_style(
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(outer_block, picker_area);

    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    if picker.sessions.is_empty() {
        let empty_msg = Paragraph::new("No sessions found")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        frame.render_widget(empty_msg, inner_area);
        return;
    }

    let list_height = inner_area.height.saturating_sub(2) as usize;

    let list_area = Rect::new(
        inner_area.x,
        inner_area.y,
        inner_area.width,
        list_height as u16,
    );

    let items: Vec<ListItem> = picker
        .sessions
        .iter()
        .skip(picker.offset)
        .take(list_height)
        .map(|session| {
            let timestamp = session
                .modified
                .and_then(session::format_timestamp)
                .unwrap_or_else(|| "unknown".to_string());

            let display_title = truncate_with_ellipsis(
                &session.display_title(),
                (inner_area.width as usize).saturating_sub(20),
            );
            let short_id = short_session_id(&session.id);

            let line = Line::from(vec![
                Span::styled(short_id, Style::default().fg(Color::Cyan)),
                Span::styled("  ", Style::default()),
                Span::styled(display_title, Style::default().fg(Color::White)),
                Span::styled("  ", Style::default()),
                Span::styled(timestamp, Style::default().fg(Color::DarkGray)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    let visible_selected = picker.selected.saturating_sub(picker.offset);
    list_state.select(Some(visible_selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    let separator = "─".repeat(inner_area.width as usize);
    let sep_y = inner_area.y + list_height as u16;
    if sep_y < inner_area.y + inner_area.height {
        let separator_area = Rect::new(inner_area.x, sep_y, inner_area.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                &separator,
                Style::default().fg(Color::DarkGray),
            ))),
            separator_area,
        );
    }

    let hints_y = inner_area.y + inner_area.height.saturating_sub(1);
    let hints_area = Rect::new(inner_area.x, hints_y, inner_area.width, 1);
    let hints_line = Line::from(vec![
        Span::styled("↑↓", Style::default().fg(Color::Blue)),
        Span::styled(" navigate ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Enter", Style::default().fg(Color::Blue)),
        Span::styled(" select ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Esc", Style::default().fg(Color::Blue)),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ]);
    let hints_para = Paragraph::new(hints_line).alignment(Alignment::Center);
    frame.render_widget(hints_para, hints_area);
}

fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let mut truncated = String::new();
    for ch in text.chars() {
        let next_width = truncated.width() + ch.width().unwrap_or(0);
        if next_width + 1 > max_width {
            break;
        }
        truncated.push(ch);
    }
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_picker_state_new_empty() {
        let (state, _) = SessionPickerState::open(vec![], vec![]);
        assert_eq!(state.selected, 0);
        assert_eq!(state.offset, 0);
        assert!(state.sessions.is_empty());
        assert!(state.original_cells.is_empty());
        assert!(state.selected_session().is_none());
    }

    #[test]
    fn test_session_picker_state_new_with_sessions() {
        let sessions = vec![
            SessionSummary {
                id: "session-1".to_string(),
                title: None,
                modified: None,
            },
            SessionSummary {
                id: "session-2".to_string(),
                title: None,
                modified: None,
            },
        ];
        let (state, _) = SessionPickerState::open(sessions, vec![]);
        assert_eq!(state.selected, 0);
        assert_eq!(state.sessions.len(), 2);
        assert_eq!(state.selected_session().unwrap().id, "session-1");
    }

    #[test]
    fn test_session_picker_stores_original_cells() {
        let sessions = vec![SessionSummary {
            id: "s1".to_string(),
            title: None,
            modified: None,
        }];
        let original_cells = vec![
            HistoryCell::user("test message"),
            HistoryCell::assistant("response"),
        ];
        let (state, _) = SessionPickerState::open(sessions, original_cells.clone());
        assert_eq!(state.original_cells.len(), 2);
    }

    #[test]
    fn test_navigation_bounds() {
        let sessions = vec![
            SessionSummary {
                id: "s1".to_string(),
                title: None,
                modified: None,
            },
            SessionSummary {
                id: "s2".to_string(),
                title: None,
                modified: None,
            },
            SessionSummary {
                id: "s3".to_string(),
                title: None,
                modified: None,
            },
        ];
        let (mut state, _) = SessionPickerState::open(sessions, vec![]);

        assert_eq!(state.selected, 0);

        state.selected = 1;
        assert_eq!(state.selected, 1);

        state.selected = 2;
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn test_scroll_offset_down() {
        let (mut picker, _) = SessionPickerState::open(
            (0..15)
                .map(|i| SessionSummary {
                    id: format!("session-{}", i),
                    title: None,
                    modified: None,
                })
                .collect(),
            vec![],
        );

        assert_eq!(picker.selected, 0);
        assert_eq!(picker.offset, 0);

        for i in 1..=10 {
            picker.selected = i;
            if picker.selected >= picker.offset + VISIBLE_HEIGHT {
                picker.offset = picker.selected - VISIBLE_HEIGHT + 1;
            }
        }

        assert_eq!(picker.selected, 10);
        assert_eq!(picker.offset, 3);
    }

    #[test]
    fn test_scroll_offset_up() {
        let (mut picker, _) = SessionPickerState::open(
            (0..15)
                .map(|i| SessionSummary {
                    id: format!("session-{}", i),
                    title: None,
                    modified: None,
                })
                .collect(),
            vec![],
        );

        picker.selected = 10;
        picker.offset = 5;

        picker.selected = 3;
        if picker.selected < picker.offset {
            picker.offset = picker.selected;
        }

        assert_eq!(picker.selected, 3);
        assert_eq!(picker.offset, 3);
    }
}
