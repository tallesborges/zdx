use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::OverlayAction;
use crate::core::session::SessionSummary;
use crate::modes::tui::app::TuiState;
use crate::modes::tui::session::render_session_picker;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{StateCommand, TranscriptCommand};
use crate::modes::tui::transcript::HistoryCell;

const VISIBLE_HEIGHT: usize = 8; // MAX_VISIBLE_SESSIONS - 2
const COPIED_FEEDBACK_DURATION_MS: u128 = 300;

#[derive(Debug, Clone)]
pub struct SessionPickerState {
    pub sessions: Vec<SessionSummary>,
    pub selected: usize,
    pub offset: usize,
    pub original_cells: Vec<HistoryCell>,
    /// When the last copy occurred (for showing brief "Copied!" feedback).
    pub copied_at: Option<Instant>,
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
            copied_at: None,
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
        // Delegate to session feature view
        render_session_picker(frame, self, area, input_y)
    }

    pub fn handle_key(
        &mut self,
        tui: &TuiState,
        key: KeyEvent,
    ) -> (Option<OverlayAction>, Vec<StateCommand>) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        let (action, commands) = match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => (
                Some(OverlayAction::close()),
                vec![
                    StateCommand::Transcript(TranscriptCommand::ReplaceCells(
                        self.original_cells.clone(),
                    )),
                    StateCommand::Transcript(TranscriptCommand::ResetScroll),
                    StateCommand::Transcript(TranscriptCommand::ClearWrapCache),
                ],
            ),
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                    if self.selected < self.offset {
                        self.offset = self.selected;
                    }
                }
                (
                    self.selected_session().map(|session| {
                        OverlayAction::Effects(vec![UiEffect::PreviewSession {
                            session_id: session.id.clone(),
                        }])
                    }),
                    vec![],
                )
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected < self.sessions.len().saturating_sub(1) {
                    self.selected += 1;
                    if self.selected >= self.offset + VISIBLE_HEIGHT {
                        self.offset = self.selected - VISIBLE_HEIGHT + 1;
                    }
                }
                (
                    self.selected_session().map(|session| {
                        OverlayAction::Effects(vec![UiEffect::PreviewSession {
                            session_id: session.id.clone(),
                        }])
                    }),
                    vec![],
                )
            }
            KeyCode::Enter => {
                if tui.agent_state.is_running() {
                    return (
                        None,
                        vec![StateCommand::Transcript(
                            TranscriptCommand::AppendSystemMessage(
                                "Stop the current task first.".to_string(),
                            ),
                        )],
                    );
                }

                if let Some(session) = self.selected_session() {
                    (
                        Some(OverlayAction::close_with(vec![UiEffect::LoadSession {
                            session_id: session.id.clone(),
                        }])),
                        vec![],
                    )
                } else {
                    (Some(OverlayAction::close()), vec![])
                }
            }
            KeyCode::Char('y') => {
                if let Some(session) = self.selected_session() {
                    (
                        Some(OverlayAction::Effects(vec![UiEffect::CopyToClipboard {
                            text: session.id.clone(),
                        }])),
                        vec![],
                    )
                } else {
                    (None, vec![])
                }
            }
            _ => (None, vec![]),
        };

        (action, commands)
    }

    pub fn selected_session(&self) -> Option<&SessionSummary> {
        self.sessions.get(self.selected)
    }

    /// Returns true if the "Copied!" feedback should be shown.
    pub fn should_show_copied(&self) -> bool {
        self.copied_at
            .map(|t| t.elapsed().as_millis() < COPIED_FEEDBACK_DURATION_MS)
            .unwrap_or(false)
    }
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
