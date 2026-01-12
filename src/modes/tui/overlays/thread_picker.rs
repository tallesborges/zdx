use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::OverlayUpdate;
use crate::core::thread_log::ThreadSummary;
use crate::modes::tui::app::TuiState;
use crate::modes::tui::shared::LatestOnly;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{StateMutation, TranscriptMutation};
use crate::modes::tui::thread::render_thread_picker;
use crate::modes::tui::transcript::HistoryCell;

const VISIBLE_HEIGHT: usize = 8; // MAX_VISIBLE_THREADS - 2
const COPIED_FEEDBACK_DURATION_MS: u128 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadScope {
    Current,
    All,
}

impl ThreadScope {
    fn toggle(self) -> Self {
        match self {
            ThreadScope::Current => ThreadScope::All,
            ThreadScope::All => ThreadScope::Current,
        }
    }
}

#[derive(Debug)]
pub struct ThreadPickerState {
    pub all_threads: Vec<ThreadSummary>,
    pub scope: ThreadScope,
    pub current_root: String,
    pub selected: usize,
    pub offset: usize,
    pub original_cells: Vec<HistoryCell>,
    /// When the last copy occurred (for showing brief "Copied!" feedback).
    pub copied_at: Option<Instant>,
    /// Tracks the latest preview request.
    pub preview_request: LatestOnly,
}

impl ThreadPickerState {
    pub fn open(
        threads: Vec<ThreadSummary>,
        original_cells: Vec<HistoryCell>,
        current_root: &std::path::Path,
    ) -> (Self, Vec<UiEffect>) {
        let current_root = current_root
            .canonicalize()
            .unwrap_or_else(|_| current_root.to_path_buf())
            .display()
            .to_string();
        let mut state = Self {
            all_threads: threads,
            scope: ThreadScope::Current,
            current_root,
            selected: 0,
            offset: 0,
            original_cells,
            copied_at: None,
            preview_request: LatestOnly::default(),
        };
        let effects = state.preview_selected_effects();
        (state, effects)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        // Delegate to thread feature view
        render_thread_picker(frame, self, area, input_y)
    }

    pub fn handle_key(&mut self, tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Char('t') if ctrl => {
                self.scope = self.scope.toggle();
                self.selected = 0;
                self.offset = 0;
                OverlayUpdate::stay().with_ui_effects(self.preview_selected_effects())
            }
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                OverlayUpdate::close().with_mutations(vec![
                    StateMutation::Transcript(TranscriptMutation::ReplaceCells(
                        self.original_cells.clone(),
                    )),
                    StateMutation::Transcript(TranscriptMutation::ResetScroll),
                    StateMutation::Transcript(TranscriptMutation::ClearWrapCache),
                ])
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let total = self.visible_threads().len();
                if self.selected > 0 {
                    self.selected -= 1;
                    if self.selected < self.offset {
                        self.offset = self.selected;
                    }
                }
                self.selected = self.selected.min(total.saturating_sub(1));
                OverlayUpdate::stay().with_ui_effects(self.preview_selected_effects())
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let total = self.visible_threads().len();
                if self.selected < total.saturating_sub(1) {
                    self.selected += 1;
                    if self.selected >= self.offset + VISIBLE_HEIGHT {
                        self.offset = self.selected - VISIBLE_HEIGHT + 1;
                    }
                }
                OverlayUpdate::stay().with_ui_effects(self.preview_selected_effects())
            }
            KeyCode::Enter => {
                if tui.agent_state.is_running() {
                    return OverlayUpdate::stay().with_mutations(vec![StateMutation::Transcript(
                        TranscriptMutation::AppendSystemMessage(
                            "Stop the current task first.".to_string(),
                        ),
                    )]);
                }

                if let Some(thread) = self.selected_thread() {
                    if tui.tasks.thread_load.is_running() {
                        return OverlayUpdate::stay();
                    }
                    OverlayUpdate::close()
                        .with_ui_effects(vec![UiEffect::LoadThread {
                            task: None,
                            thread_id: thread.id.clone(),
                        }])
                        .with_mutations(vec![])
                } else {
                    OverlayUpdate::close()
                }
            }
            KeyCode::Char('y') => {
                if let Some(thread) = self.selected_thread() {
                    OverlayUpdate::stay().with_ui_effects(vec![UiEffect::CopyToClipboard {
                        text: thread.id.clone(),
                    }])
                } else {
                    OverlayUpdate::stay()
                }
            }
            _ => OverlayUpdate::stay(),
        }
    }

    pub fn selected_thread(&self) -> Option<&ThreadSummary> {
        self.visible_threads().get(self.selected).copied()
    }

    /// Returns true if the "Copied!" feedback should be shown.
    pub fn should_show_copied(&self) -> bool {
        self.copied_at
            .map(|t| t.elapsed().as_millis() < COPIED_FEEDBACK_DURATION_MS)
            .unwrap_or(false)
    }

    pub fn visible_threads(&self) -> Vec<&ThreadSummary> {
        match self.scope {
            ThreadScope::All => self.all_threads.iter().collect(),
            ThreadScope::Current => self
                .all_threads
                .iter()
                .filter(|thread| thread.root_path.as_deref() == Some(self.current_root.as_str()))
                .collect(),
        }
    }

    fn preview_selected_effects(&mut self) -> Vec<UiEffect> {
        let thread_id = self.selected_thread().map(|thread| thread.id.clone());
        if let Some(thread_id) = thread_id {
            let req = self.preview_request.begin();
            vec![UiEffect::PreviewThread {
                task: None,
                thread_id,
                req,
            }]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_picker_state_new_empty() {
        let (state, _) = ThreadPickerState::open(vec![], vec![], std::path::Path::new("."));
        assert_eq!(state.selected, 0);
        assert_eq!(state.offset, 0);
        assert!(state.all_threads.is_empty());
        assert!(state.original_cells.is_empty());
        assert!(state.selected_thread().is_none());
    }

    #[test]
    fn test_thread_picker_state_new_with_threads() {
        let current_root = std::path::Path::new(".")
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .display()
            .to_string();
        let threads = vec![
            ThreadSummary {
                id: "thread-1".to_string(),
                title: None,
                root_path: Some(current_root.clone()),
                modified: None,
            },
            ThreadSummary {
                id: "thread-2".to_string(),
                title: None,
                root_path: Some(current_root),
                modified: None,
            },
        ];
        let (state, _) = ThreadPickerState::open(threads, vec![], std::path::Path::new("."));
        assert_eq!(state.selected, 0);
        assert_eq!(state.all_threads.len(), 2);
        assert_eq!(state.selected_thread().unwrap().id, "thread-1");
    }

    #[test]
    fn test_thread_picker_stores_original_cells() {
        let threads = vec![ThreadSummary {
            id: "s1".to_string(),
            title: None,
            root_path: None,
            modified: None,
        }];
        let original_cells = vec![
            HistoryCell::user("test message"),
            HistoryCell::assistant("response"),
        ];
        let (state, _) =
            ThreadPickerState::open(threads, original_cells.clone(), std::path::Path::new("."));
        assert_eq!(state.original_cells.len(), 2);
    }

    #[test]
    fn test_navigation_bounds() {
        let threads = vec![
            ThreadSummary {
                id: "s1".to_string(),
                title: None,
                root_path: None,
                modified: None,
            },
            ThreadSummary {
                id: "s2".to_string(),
                title: None,
                root_path: None,
                modified: None,
            },
            ThreadSummary {
                id: "s3".to_string(),
                title: None,
                root_path: None,
                modified: None,
            },
        ];
        let (mut state, _) = ThreadPickerState::open(threads, vec![], std::path::Path::new("."));

        assert_eq!(state.selected, 0);

        state.selected = 1;
        assert_eq!(state.selected, 1);

        state.selected = 2;
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn test_scroll_offset_down() {
        let (mut picker, _) = ThreadPickerState::open(
            (0..15)
                .map(|i| ThreadSummary {
                    id: format!("thread-{}", i),
                    title: None,
                    root_path: None,
                    modified: None,
                })
                .collect(),
            vec![],
            std::path::Path::new("."),
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
        let (mut picker, _) = ThreadPickerState::open(
            (0..15)
                .map(|i| ThreadSummary {
                    id: format!("thread-{}", i),
                    title: None,
                    root_path: None,
                    modified: None,
                })
                .collect(),
            vec![],
            std::path::Path::new("."),
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
