use std::collections::HashSet;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use zdx_core::core::thread_persistence::ThreadSummary;

use super::OverlayUpdate;
use crate::common::TaskKind;
use crate::effects::UiEffect;
use crate::input::InputState;
use crate::mutations::{InputMutation, StateMutation, TranscriptMutation};
use crate::state::TuiState;
use crate::thread::{MAX_VISIBLE_THREADS, ThreadDisplayItem, render_thread_picker};
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
    pub active_thread_ids: HashSet<String>,
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
        active_thread_ids: HashSet<String>,
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
            active_thread_ids,
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
        render_thread_picker(frame, self, area, input_y);
    }

    pub fn handle_key(&mut self, tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        self.refresh_active_thread_ids();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Char('t') if ctrl => self.toggle_scope(),
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                self.close_overlay()
            }
            KeyCode::Up | KeyCode::Char('k') => self.navigate_up(),
            KeyCode::Down | KeyCode::Char('j') => self.navigate_down(),
            KeyCode::Enter => self.handle_enter(tui),
            KeyCode::Char('y') => self.copy_selected_thread_id(),
            // Ctrl+U: clear the filter
            KeyCode::Char('u') if ctrl && !shift && !alt => {
                self.filter.clear();
                self.clamp_selection();
                self.preview_update()
            }
            KeyCode::Backspace => {
                if alt {
                    clear_word_left(&mut self.filter);
                } else {
                    self.filter.pop();
                }
                self.clamp_selection();
                self.preview_update()
            }
            KeyCode::Char(c) if !ctrl => {
                self.filter.push(c);
                self.clamp_selection();
                self.preview_update()
            }
            _ => OverlayUpdate::stay(),
        }
    }

    fn toggle_scope(&mut self) -> OverlayUpdate {
        self.scope = self.scope.toggle();
        self.selected = 0;
        self.offset = 0;
        self.preview_update()
    }

    fn close_overlay(&self) -> OverlayUpdate {
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

    fn navigate_up(&mut self) -> OverlayUpdate {
        let total = self.visible_tree_items().len();
        if self.selected > 0 {
            self.selected -= 1;
            if self.selected < self.offset {
                self.offset = self.selected;
            }
        }
        self.selected = self.selected.min(total.saturating_sub(1));
        self.preview_update()
    }

    fn navigate_down(&mut self) -> OverlayUpdate {
        let total = self.visible_tree_items().len();
        let visible_height = self.visible_height();
        if self.selected < total.saturating_sub(1) {
            self.selected += 1;
            if self.selected >= self.offset + visible_height {
                self.offset = self.selected - visible_height + 1;
            }
        }
        self.preview_update()
    }

    fn handle_enter(&self, tui: &TuiState) -> OverlayUpdate {
        match self.mode {
            ThreadPickerMode::Switch => self.switch_to_selected_thread(tui),
            ThreadPickerMode::Insert { .. } => self.insert_selected_thread(tui),
        }
    }

    fn switch_to_selected_thread(&self, tui: &TuiState) -> OverlayUpdate {
        if tui.agent_state.is_running() {
            return OverlayUpdate::stay().with_mutations(vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage("Stop the current task first.".to_string()),
            )]);
        }

        if let Some(thread) = self.selected_thread()
            && self.active_thread_ids.contains(&thread.id)
        {
            return OverlayUpdate::stay().with_mutations(vec![StateMutation::Transcript(
                TranscriptMutation::AppendSystemMessage(
                    "This thread is still running in the background. Wait for it to finish before opening it.".to_string(),
                ),
            )]);
        }

        if tui.tasks.state(TaskKind::ThreadLoad).is_running() {
            return OverlayUpdate::stay();
        }

        if let Some(thread) = self.selected_thread() {
            OverlayUpdate::close().with_ui_effects(vec![UiEffect::LoadThread {
                thread_id: thread.id.clone(),
            }])
        } else {
            OverlayUpdate::close()
        }
    }

    fn insert_selected_thread(&self, tui: &TuiState) -> OverlayUpdate {
        let mut mutations = Vec::new();
        if let Some(mutation) = self.select_thread_and_insert(&tui.input) {
            mutations.push(StateMutation::Input(mutation));
        }
        OverlayUpdate::close().with_mutations(mutations)
    }

    fn copy_selected_thread_id(&self) -> OverlayUpdate {
        if let Some(thread) = self.selected_thread() {
            OverlayUpdate::stay().with_ui_effects(vec![UiEffect::CopyToClipboard {
                text: thread.id.clone(),
            }])
        } else {
            OverlayUpdate::stay()
        }
    }

    pub fn selected_thread(&self) -> Option<&ThreadSummary> {
        self.visible_tree_items()
            .get(self.selected)
            .map(|item| item.summary)
    }

    pub fn is_thread_active(&self, thread_id: &str) -> bool {
        self.active_thread_ids.contains(thread_id)
    }

    fn refresh_active_thread_ids(&mut self) {
        self.active_thread_ids = zdx_core::agent_activity::list_active()
            .into_iter()
            .filter_map(|run| run.thread_id)
            .collect();
    }

    /// Returns true if the "Copied!" feedback should be shown.
    pub fn should_show_copied(&self) -> bool {
        self.copied_at
            .is_some_and(|t| t.elapsed().as_millis() < COPIED_FEEDBACK_DURATION_MS)
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

        // Apply fuzzy search filter
        if self.filter.is_empty() {
            scoped
        } else {
            let mut ranked: Vec<_> = scoped
                .into_iter()
                .filter_map(|thread| {
                    thread_fuzzy_score(thread, &self.filter).map(|score| (thread, score))
                })
                .collect();
            ranked.sort_by(|(_, a), (_, b)| b.cmp(a));
            ranked.into_iter().map(|(thread, _)| thread).collect()
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
    /// `MAX_VISIBLE_THREADS`. This ensures scroll logic matches render layout.
    fn visible_height(&self) -> usize {
        let count = self.visible_tree_items().len();
        if count == 0 {
            1
        } else {
            count.min(MAX_VISIBLE_THREADS)
        }
    }

    /// Returns the visible threads prepared for display in the history overlay.
    ///
    /// Handoff threads are still marked, but the picker keeps the original list
    /// order instead of reorganizing them into a parent/child tree.
    pub fn visible_tree_items(&self) -> Vec<ThreadDisplayItem<'_>> {
        self.visible_threads()
            .into_iter()
            .map(|summary| ThreadDisplayItem {
                summary,
                depth: 0,
                is_handoff: summary.handoff_from.is_some(),
            })
            .collect()
    }

    fn preview_selected_effects(&mut self) -> Vec<UiEffect> {
        if !self.mode.is_switch() {
            return Vec::new();
        }
        let thread_id = self.selected_thread().map(|thread| thread.id.clone());
        if let Some(thread_id) = thread_id {
            if self.active_thread_ids.contains(&thread_id) {
                return Vec::new();
            }
            vec![UiEffect::PreviewThread { thread_id }]
        } else {
            Vec::new()
        }
    }

    fn preview_update(&mut self) -> OverlayUpdate {
        let effects = self.preview_selected_effects();
        if effects.is_empty() && self.mode.is_switch() {
            OverlayUpdate::stay().with_mutations(vec![
                StateMutation::Transcript(TranscriptMutation::ReplaceCells(
                    self.original_cells.clone(),
                )),
                StateMutation::Transcript(TranscriptMutation::ResetScroll),
                StateMutation::Transcript(TranscriptMutation::ClearWrapCache),
            ])
        } else {
            OverlayUpdate::stay().with_ui_effects(effects)
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
            target_col = new_lines.last().map_or(0, |l| l.len());
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

/// Returns a fuzzy match score if the thread matches the filter, or `None` if no match.
///
/// Matches against thread ID and title using nucleo fuzzy matching.
fn thread_fuzzy_score(thread: &ThreadSummary, filter: &str) -> Option<u32> {
    use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
    use nucleo_matcher::{Config, Matcher, Utf32Str};

    if filter.is_empty() {
        return Some(0);
    }

    let pattern = Pattern::parse(filter, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);

    let id_score = {
        let mut buf = Vec::new();
        let haystack = Utf32Str::new(&thread.id, &mut buf);
        pattern.score(haystack, &mut matcher)
    };

    let title_score = thread.title.as_deref().and_then(|t| {
        let mut buf = Vec::new();
        let haystack = Utf32Str::new(t, &mut buf);
        pattern.score(haystack, &mut matcher)
    });

    // Take the best score from either field
    match (id_score, title_score) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
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
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn test_thread_picker_state_new_empty() {
        let (state, _) = ThreadPickerState::open(
            vec![],
            HashSet::new(),
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
            HashSet::new(),
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
            HashSet::new(),
            original_cells.clone(),
            std::path::Path::new("."),
            None,
            ThreadPickerMode::Switch,
        );
        assert_eq!(state.original_cells.len(), 2);
    }

    #[test]
    fn test_visible_tree_items_keep_handoffs_in_original_order() {
        let current_root = std::path::Path::new(".")
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .display()
            .to_string();
        let threads = vec![
            ThreadSummary {
                id: "thread-b".to_string(),
                title: Some("Child".to_string()),
                root_path: Some(current_root.clone()),
                modified: None,
                handoff_from: Some("thread-a".to_string()),
            },
            ThreadSummary {
                id: "thread-a".to_string(),
                title: Some("Parent".to_string()),
                root_path: Some(current_root),
                modified: None,
                handoff_from: None,
            },
        ];
        let (state, _) = ThreadPickerState::open(
            threads,
            HashSet::new(),
            vec![],
            std::path::Path::new("."),
            None,
            ThreadPickerMode::Switch,
        );

        let items = state.visible_tree_items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].summary.id, "thread-b");
        assert_eq!(items[1].summary.id, "thread-a");
        assert_eq!(items[0].depth, 0);
        assert_eq!(items[1].depth, 0);
        assert!(items[0].is_handoff);
        assert!(!items[1].is_handoff);
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
            HashSet::new(),
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
                    id: format!("thread-{i}"),
                    title: None,
                    root_path: None,
                    modified: None,
                    handoff_from: None,
                })
                .collect(),
            HashSet::new(),
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
                    id: format!("thread-{i}"),
                    title: None,
                    root_path: None,
                    modified: None,
                    handoff_from: None,
                })
                .collect(),
            HashSet::new(),
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
