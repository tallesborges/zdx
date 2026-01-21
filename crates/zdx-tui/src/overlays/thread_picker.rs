use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use zdx_core::core::thread_log::ThreadSummary;

use super::OverlayUpdate;
use crate::effects::UiEffect;
use crate::input::InputState;
use crate::mutations::{InputMutation, StateMutation, TranscriptMutation};
use crate::state::TuiState;
use crate::thread::{
    MAX_VISIBLE_THREADS, ThreadDisplayItem, flatten_refs_as_tree, render_thread_picker,
};
use crate::transcript::HistoryCell;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadPickerMode {
    Switch,
    Insert { trigger_pos: usize },
}

impl ThreadPickerMode {
    pub fn is_switch(self) -> bool {
        matches!(self, ThreadPickerMode::Switch)
    }

    fn trigger_pos(self) -> Option<usize> {
        match self {
            ThreadPickerMode::Insert { trigger_pos } => Some(trigger_pos),
            ThreadPickerMode::Switch => None,
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
    pub mode: ThreadPickerMode,
    /// When the last copy occurred (for showing brief "Copied!" feedback).
    pub copied_at: Option<Instant>,
    /// ID of the currently active thread (for highlighting in picker).
    pub current_thread_id: Option<String>,
    /// Search filter text (filters by thread ID or title).
    pub filter: String,
}

impl ThreadPickerState {
    pub fn open(
        threads: Vec<ThreadSummary>,
        original_cells: Vec<HistoryCell>,
        current_root: &std::path::Path,
        current_thread_id: Option<String>,
        mode: ThreadPickerMode,
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
            mode,
            copied_at: None,
            current_thread_id,
            filter: String::new(),
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
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Char('t') if ctrl => {
                self.scope = self.scope.toggle();
                self.selected = 0;
                self.offset = 0;
                OverlayUpdate::stay().with_ui_effects(self.preview_selected_effects())
            }
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                if self.mode.is_switch() {
                    OverlayUpdate::close().with_mutations(vec![
                        StateMutation::Transcript(TranscriptMutation::ReplaceCells(
                            self.original_cells.clone(),
                        )),
                        StateMutation::Transcript(TranscriptMutation::ResetScroll),
                        StateMutation::Transcript(TranscriptMutation::ClearWrapCache),
                    ])
                } else {
                    OverlayUpdate::close()
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let total = self.visible_tree_items().len();
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
                let total = self.visible_tree_items().len();
                let visible_height = self.visible_height();
                if self.selected < total.saturating_sub(1) {
                    self.selected += 1;
                    if self.selected >= self.offset + visible_height {
                        self.offset = self.selected - visible_height + 1;
                    }
                }
                OverlayUpdate::stay().with_ui_effects(self.preview_selected_effects())
            }
            KeyCode::Enter => match self.mode {
                ThreadPickerMode::Switch => {
                    if tui.agent_state.is_running() {
                        return OverlayUpdate::stay().with_mutations(vec![
                            StateMutation::Transcript(TranscriptMutation::AppendSystemMessage(
                                "Stop the current task first.".to_string(),
                            )),
                        ]);
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
                ThreadPickerMode::Insert { .. } => {
                    let mut mutations = Vec::new();
                    if let Some(mutation) = self.select_thread_and_insert(&tui.input) {
                        mutations.push(StateMutation::Input(mutation));
                    }
                    OverlayUpdate::close().with_mutations(mutations)
                }
            },
            KeyCode::Char('y') => {
                if let Some(thread) = self.selected_thread() {
                    OverlayUpdate::stay().with_ui_effects(vec![UiEffect::CopyToClipboard {
                        text: thread.id.clone(),
                    }])
                } else {
                    OverlayUpdate::stay()
                }
            }
            // Ctrl+U: clear the filter
            KeyCode::Char('u') if ctrl && !shift && !alt => {
                self.filter.clear();
                self.clamp_selection();
                OverlayUpdate::stay().with_ui_effects(self.preview_selected_effects())
            }
            KeyCode::Backspace => {
                if alt {
                    clear_word_left(&mut self.filter);
                } else {
                    self.filter.pop();
                }
                self.clamp_selection();
                OverlayUpdate::stay().with_ui_effects(self.preview_selected_effects())
            }
            KeyCode::Char(c) if !ctrl => {
                self.filter.push(c);
                self.clamp_selection();
                OverlayUpdate::stay().with_ui_effects(self.preview_selected_effects())
            }
            _ => OverlayUpdate::stay(),
        }
    }

    pub fn selected_thread(&self) -> Option<&ThreadSummary> {
        self.visible_tree_items()
            .get(self.selected)
            .map(|item| item.summary)
    }

    /// Returns true if the "Copied!" feedback should be shown.
    pub fn should_show_copied(&self) -> bool {
        self.copied_at
            .map(|t| t.elapsed().as_millis() < COPIED_FEEDBACK_DURATION_MS)
            .unwrap_or(false)
    }

    pub fn visible_threads(&self) -> Vec<&ThreadSummary> {
        let scoped: Vec<_> = match self.scope {
            ThreadScope::All => self.all_threads.iter().collect(),
            ThreadScope::Current => self
                .all_threads
                .iter()
                .filter(|thread| thread.root_path.as_deref() == Some(self.current_root.as_str()))
                .collect(),
        };

        // Apply search filter
        if self.filter.is_empty() {
            scoped
        } else {
            scoped
                .into_iter()
                .filter(|thread| thread_matches_filter(thread, &self.filter))
                .collect()
        }
    }

    /// Clamps the selection index to valid range after filter changes.
    fn clamp_selection(&mut self) {
        let count = self.visible_tree_items().len();
        if count == 0 {
            self.selected = 0;
            self.offset = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
            // Also adjust offset if needed
            let visible_height = self.visible_height();
            if self.selected < self.offset {
                self.offset = self.selected;
            } else if self.selected >= self.offset + visible_height {
                self.offset = self.selected - visible_height + 1;
            }
        }
    }

    /// Returns the number of threads visible in the picker list.
    ///
    /// Calculated dynamically based on the filtered thread count, capped at
    /// MAX_VISIBLE_THREADS. This ensures scroll logic matches render layout.
    fn visible_height(&self) -> usize {
        let count = self.visible_tree_items().len();
        if count == 0 {
            1
        } else {
            count.min(MAX_VISIBLE_THREADS)
        }
    }

    /// Returns the visible threads as a tree-flattened list for hierarchical display.
    ///
    /// Delegates to the shared tree utility. The returned items contain depth
    /// information for rendering indentation.
    pub fn visible_tree_items(&self) -> Vec<ThreadDisplayItem<'_>> {
        let visible = self.visible_threads();
        flatten_refs_as_tree(&visible)
    }

    fn preview_selected_effects(&mut self) -> Vec<UiEffect> {
        if !self.mode.is_switch() {
            return Vec::new();
        }
        let thread_id = self.selected_thread().map(|thread| thread.id.clone());
        if let Some(thread_id) = thread_id {
            vec![UiEffect::PreviewThread {
                task: None,
                thread_id,
            }]
        } else {
            Vec::new()
        }
    }

    fn select_thread_and_insert(&self, input: &InputState) -> Option<InputMutation> {
        let selected = self.selected_thread()?;
        let trigger_pos = self.mode.trigger_pos()?;

        let text = input.get_text();
        let cursor_byte_pos = Self::get_cursor_byte_pos(input);

        let before_at = &text[..=trigger_pos];
        let after_cursor = if cursor_byte_pos < text.len() {
            &text[cursor_byte_pos..]
        } else {
            ""
        };

        let new_text = format!("{}@{} {}", before_at, selected.id, after_cursor);
        let new_cursor_byte_pos = trigger_pos + 2 + selected.id.len() + 1;

        let new_lines: Vec<&str> = new_text.lines().collect();
        let mut remaining = new_cursor_byte_pos;
        let mut target_row = 0;
        let mut target_col = 0;

        for (i, line) in new_lines.iter().enumerate() {
            if remaining <= line.len() {
                target_row = i;
                target_col = remaining;
                break;
            }
            remaining -= line.len() + 1;
            target_row = i + 1;
            target_col = 0;
        }

        if target_row >= new_lines.len() {
            target_row = new_lines.len().saturating_sub(1);
            target_col = new_lines.last().map(|l| l.len()).unwrap_or(0);
        }

        Some(InputMutation::SetTextAndCursor {
            text: new_text,
            cursor_row: target_row,
            cursor_col: target_col,
        })
    }

    fn get_cursor_byte_pos(input: &InputState) -> usize {
        let text = input.get_text();
        let (row, col) = input.textarea.cursor();
        let lines: Vec<&str> = text.lines().collect();

        let mut pos = 0;
        for (i, line) in lines.iter().enumerate() {
            if i < row {
                pos += line.len() + 1;
            } else {
                pos += col;
                break;
            }
        }
        pos
    }
}

/// Returns true if the thread matches the given filter text.
///
/// Matches against thread ID (case-insensitive) and title (case-insensitive).
fn thread_matches_filter(thread: &ThreadSummary, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }

    let filter_lower = filter.to_lowercase();
    let id_lower = thread.id.to_lowercase();

    // Match against ID
    if id_lower.contains(&filter_lower) {
        return true;
    }

    // Match against title if present
    if let Some(title) = &thread.title
        && title.to_lowercase().contains(&filter_lower)
    {
        return true;
    }

    false
}

/// Clears characters from the end of the string back to the previous word boundary.
fn clear_word_left(input: &mut String) {
    let trimmed_len = input.trim_end().len();
    if trimmed_len == 0 {
        input.clear();
        return;
    }

    input.truncate(trimmed_len);
    let mut chars: Vec<char> = input.chars().collect();
    while let Some(&ch) = chars.last() {
        if ch.is_whitespace() {
            break;
        }
        chars.pop();
    }
    input.clear();
    input.extend(chars);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_picker_state_new_empty() {
        let (state, _) = ThreadPickerState::open(
            vec![],
            vec![],
            std::path::Path::new("."),
            None,
            ThreadPickerMode::Switch,
        );
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
                handoff_from: None,
            },
            ThreadSummary {
                id: "thread-2".to_string(),
                title: None,
                root_path: Some(current_root),
                modified: None,
                handoff_from: None,
            },
        ];
        let (state, _) = ThreadPickerState::open(
            threads,
            vec![],
            std::path::Path::new("."),
            None,
            ThreadPickerMode::Switch,
        );
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
            handoff_from: None,
        }];
        let original_cells = vec![
            HistoryCell::user("test message"),
            HistoryCell::assistant("response"),
        ];
        let (state, _) = ThreadPickerState::open(
            threads,
            original_cells.clone(),
            std::path::Path::new("."),
            None,
            ThreadPickerMode::Switch,
        );
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
                handoff_from: None,
            },
            ThreadSummary {
                id: "s2".to_string(),
                title: None,
                root_path: None,
                modified: None,
                handoff_from: None,
            },
            ThreadSummary {
                id: "s3".to_string(),
                title: None,
                root_path: None,
                modified: None,
                handoff_from: None,
            },
        ];
        let (mut state, _) = ThreadPickerState::open(
            threads,
            vec![],
            std::path::Path::new("."),
            None,
            ThreadPickerMode::Switch,
        );

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
                    handoff_from: None,
                })
                .collect(),
            vec![],
            std::path::Path::new("."),
            None,
            ThreadPickerMode::Switch,
        );

        // Switch to All scope so threads without root_path are visible
        picker.scope = ThreadScope::All;

        assert_eq!(picker.selected, 0);
        assert_eq!(picker.offset, 0);

        let visible_height = picker.visible_height();
        for i in 1..=12 {
            picker.selected = i;
            if picker.selected >= picker.offset + visible_height {
                picker.offset = picker.selected - visible_height + 1;
            }
        }

        assert_eq!(picker.selected, 12);
        // With MAX_VISIBLE_THREADS=10, offset should be 3
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
                    handoff_from: None,
                })
                .collect(),
            vec![],
            std::path::Path::new("."),
            None,
            ThreadPickerMode::Switch,
        );

        // Switch to All scope so threads without root_path are visible
        picker.scope = ThreadScope::All;

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
