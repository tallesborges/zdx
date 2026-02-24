#![allow(
    clippy::match_same_arms,
    clippy::cast_possible_truncation,
    clippy::too_many_lines
)]

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use tokio_util::sync::CancellationToken;

use super::OverlayUpdate;
use crate::effects::UiEffect;
use crate::input::InputState;
use crate::mutations::{InputMutation, StateMutation};

const MAX_VISIBLE_FILES: usize = 10;
const VISIBLE_HEIGHT: usize = MAX_VISIBLE_FILES - 2;
const MAX_DEPTH: usize = 15;

/// A matched file with its score and matched character indices.
#[derive(Debug, Clone)]
pub struct FileMatch {
    /// Index into the files Vec.
    pub file_idx: usize,
    /// Match score (higher = better match). None for unfiltered results.
    pub score: Option<i64>,
    /// Byte indices of matched characters for highlighting.
    pub match_indices: Vec<usize>,
}

/// File picker state.
///
/// With the inbox pattern, file discovery results arrive via the inbox.
/// Discovery runs asynchronously; cancel is handled by the reducer via task effects.
#[derive(Debug)]
pub struct FilePickerState {
    pub trigger_pos: usize,
    pub files: Vec<PathBuf>,
    /// Filtered results with match info for scoring and highlighting.
    pub filtered: Vec<FileMatch>,
    pub selected: usize,
    pub offset: usize,
    pub loading: bool,
}

impl FilePickerState {
    pub fn open(trigger_pos: usize) -> (Self, Vec<UiEffect>) {
        (
            Self {
                trigger_pos,
                files: Vec::new(),
                filtered: Vec::new(),
                selected: 0,
                offset: 0,
                loading: true,
            },
            vec![UiEffect::DiscoverFiles],
        )
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_file_picker(frame, self, area, input_y);
    }

    pub fn handle_key(&mut self, input: &InputState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                OverlayUpdate::close()
            }
            KeyCode::Enter | KeyCode::Tab => {
                let mut mutations = Vec::new();
                if let Some(command) = self.select_file_and_insert(input) {
                    mutations.push(StateMutation::Input(command));
                }
                OverlayUpdate::close().with_mutations(mutations)
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    if self.selected < self.offset {
                        self.offset = self.selected;
                    }
                }
                OverlayUpdate::stay()
            }
            KeyCode::Down => {
                if self.selected < self.filtered.len().saturating_sub(1) {
                    self.selected += 1;
                    if self.selected >= self.offset + VISIBLE_HEIGHT {
                        self.offset = self.selected - VISIBLE_HEIGHT + 1;
                    }
                }
                OverlayUpdate::stay()
            }
            KeyCode::Char('p') if ctrl => {
                if self.selected > 0 {
                    self.selected -= 1;
                    if self.selected < self.offset {
                        self.offset = self.selected;
                    }
                }
                OverlayUpdate::stay()
            }
            KeyCode::Char('n') if ctrl => {
                if self.selected < self.filtered.len().saturating_sub(1) {
                    self.selected += 1;
                    if self.selected >= self.offset + VISIBLE_HEIGHT {
                        self.offset = self.selected - VISIBLE_HEIGHT + 1;
                    }
                }
                OverlayUpdate::stay()
            }
            _ => OverlayUpdate::stay(),
        }
    }

    pub fn selected_file(&self) -> Option<&PathBuf> {
        self.filtered
            .get(self.selected)
            .and_then(|m| self.files.get(m.file_idx))
    }

    /// Returns the match info for the currently selected file.
    pub fn selected_match(&self) -> Option<&FileMatch> {
        self.filtered.get(self.selected)
    }

    pub fn should_route_input_key(key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Tab | KeyCode::Up | KeyCode::Down => false,
            KeyCode::Char('p') if ctrl => false,
            KeyCode::Char('n') if ctrl => false,
            _ => true,
        }
    }

    pub fn apply_filter(&mut self, pattern: &str) {
        if pattern.is_empty() {
            // No filter: show all files without highlighting
            self.filtered = self
                .files
                .iter()
                .enumerate()
                .map(|(idx, _)| FileMatch {
                    file_idx: idx,
                    score: None,
                    match_indices: Vec::new(),
                })
                .collect();
        } else {
            let mut matcher = Matcher::new(Config::DEFAULT);
            let pattern = Pattern::parse(pattern, CaseMatching::Ignore, Normalization::Smart);

            // Collect matches with scores
            let mut matched_files: Vec<FileMatch> = self
                .files
                .iter()
                .enumerate()
                .filter_map(|(idx, path)| {
                    let path_str = path.to_string_lossy();
                    let mut buf = Vec::new();
                    let haystack = Utf32Str::new(&path_str, &mut buf);

                    pattern.score(haystack, &mut matcher).map(|score| {
                        // Get character indices
                        let mut char_indices = Vec::new();
                        pattern.indices(haystack, &mut matcher, &mut char_indices);

                        // Convert character indices to byte indices
                        let byte_indices = char_to_byte_indices(&path_str, &char_indices);

                        FileMatch {
                            file_idx: idx,
                            score: Some(i64::from(score)),
                            match_indices: byte_indices,
                        }
                    })
                })
                .collect();

            // Sort by score descending (best matches first)
            matched_files.sort_by_key(|m| std::cmp::Reverse(m.score.unwrap_or(i64::MIN)));

            self.filtered = matched_files;
        }

        self.selected = 0;
        self.offset = 0;
    }

    pub fn set_files(&mut self, files: Vec<PathBuf>) {
        self.files = files;
        self.loading = false;
        // Initialize filtered with all files (no highlighting)
        self.filtered = (0..self.files.len())
            .map(|idx| FileMatch {
                file_idx: idx,
                score: None,
                match_indices: Vec::new(),
            })
            .collect();
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

    fn get_filter_pattern(&self, input: &InputState) -> String {
        let text = input.get_text();
        let trigger_pos = self.trigger_pos;
        let cursor_pos = Self::get_cursor_byte_pos(input);

        if trigger_pos < text.len() && trigger_pos < cursor_pos {
            let start = trigger_pos + 1;
            let end = cursor_pos.min(text.len());
            if start <= end {
                return text[start..end].to_string();
            }
        }

        String::new()
    }

    fn is_trigger_deleted(&self, input: &InputState) -> bool {
        let text = input.get_text();
        let trigger_pos = self.trigger_pos;

        if trigger_pos >= text.len() || text.as_bytes().get(trigger_pos) != Some(&b'@') {
            return true;
        }

        let cursor_pos = Self::get_cursor_byte_pos(input);
        cursor_pos <= trigger_pos
    }

    fn select_file_and_insert(&self, input: &InputState) -> Option<InputMutation> {
        let selected_path = self.selected_file().cloned()?;

        let trigger_pos = self.trigger_pos;

        let text = input.get_text();
        let cursor_byte_pos = Self::get_cursor_byte_pos(input);

        let path_str = selected_path.to_string_lossy();
        let before_at = &text[..=trigger_pos];
        let after_cursor = if cursor_byte_pos < text.len() {
            &text[cursor_byte_pos..]
        } else {
            ""
        };

        let new_text = format!("{before_at}{path_str} {after_cursor}");

        let new_cursor_byte_pos = trigger_pos + 1 + path_str.len() + 1;

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

    pub fn update_from_input(&mut self, input: &InputState) -> bool {
        let pattern = self.get_filter_pattern(input);
        self.apply_filter(&pattern);
        self.is_trigger_deleted(input)
    }
}

/// Discovers project files, respecting .gitignore.
pub fn discover_files(root: &std::path::Path, cancel: &CancellationToken) -> Vec<PathBuf> {
    use ignore::WalkBuilder;

    let mut files = Vec::new();

    let walker = WalkBuilder::new(root)
        .standard_filters(true)
        .max_depth(Some(MAX_DEPTH))
        .build();

    for entry in walker.flatten() {
        if cancel.is_cancelled() {
            return files;
        }

        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        if let Ok(rel_path) = entry.path().strip_prefix(root) {
            if rel_path.as_os_str().is_empty() {
                continue;
            }

            files.push(rel_path.to_path_buf());
        }
    }

    files.sort();
    files
}

/// Converts character indices to byte indices.
///
/// Nucleo returns character indices, but we need byte indices for highlighting
/// since we work with byte offsets in the string.
fn char_to_byte_indices(text: &str, char_indices: &[u32]) -> Vec<usize> {
    if char_indices.is_empty() {
        return Vec::new();
    }

    let mut byte_indices = Vec::with_capacity(char_indices.len());
    let char_set: std::collections::HashSet<u32> = char_indices.iter().copied().collect();

    for (char_idx, (byte_idx, _)) in text.char_indices().enumerate() {
        if char_set.contains(&(char_idx as u32)) {
            byte_indices.push(byte_idx);
        }
    }

    byte_indices
}

/// Builds a line with highlighted matched characters.
///
/// Characters at `match_indices` are highlighted with bold + yellow,
/// other characters use the default cyan style.
fn build_highlighted_line(text: &str, match_indices: &[usize]) -> Line<'static> {
    use std::collections::HashSet;

    if match_indices.is_empty() {
        return Line::from(Span::styled(
            text.to_string(),
            Style::default().fg(Color::Cyan),
        ));
    }

    let match_set: HashSet<usize> = match_indices.iter().copied().collect();
    let mut spans = Vec::new();
    let mut current_span = String::new();
    let mut current_is_match = false;

    for (byte_idx, ch) in text.char_indices() {
        let is_match = match_set.contains(&byte_idx);

        if is_match != current_is_match && !current_span.is_empty() {
            // Style transition: push current span and start new one
            let style = if current_is_match {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan)
            };
            spans.push(Span::styled(std::mem::take(&mut current_span), style));
        }

        current_span.push(ch);
        current_is_match = is_match;
    }

    // Push the final span
    if !current_span.is_empty() {
        let style = if current_is_match {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Cyan)
        };
        spans.push(Span::styled(current_span, style));
    }

    Line::from(spans)
}

pub fn render_file_picker(
    frame: &mut Frame,
    picker: &FilePickerState,
    area: Rect,
    input_top_y: u16,
) {
    use super::render_utils::{InputHint, OverlayConfig, render_overlay, render_separator};

    let file_count = picker.filtered.len();
    let visible_count = file_count.min(MAX_VISIBLE_FILES);

    let picker_width = 50;
    let base_height = if picker.loading || file_count == 0 {
        5
    } else {
        visible_count as u16 + 4
    };
    let picker_height = base_height.max(7);

    let title = if picker.loading {
        "Files (loading...)".to_string()
    } else {
        format!("Files ({file_count})")
    };
    let hints = [
        InputHint::new("↑↓", "nav"),
        InputHint::new("Enter", "select"),
        InputHint::new("Esc", "close"),
    ];
    let layout = render_overlay(
        frame,
        area,
        input_top_y,
        &OverlayConfig {
            title: &title,
            border_color: Color::Blue,
            width: picker_width,
            height: picker_height,
            hints: &hints,
        },
    );

    if picker.loading {
        let loading_msg = Paragraph::new("Loading files...")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        frame.render_widget(loading_msg, layout.body);
        return;
    }

    if picker.filtered.is_empty() {
        let empty_msg = if picker.files.is_empty() {
            "No files found"
        } else {
            "No matches"
        };
        let msg = Paragraph::new(vec![
            Line::from(Span::styled(
                empty_msg,
                Style::default().fg(Color::DarkGray),
            )),
            Line::default(),
            Line::from(Span::styled(
                "Esc to close",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(msg, layout.body);
        return;
    }

    let list_height = layout.body.height.saturating_sub(1) as usize;
    let list_area = Rect::new(
        layout.body.x,
        layout.body.y,
        layout.body.width,
        list_height as u16,
    );

    let items: Vec<ListItem> = picker
        .filtered
        .iter()
        .skip(picker.offset)
        .take(list_height)
        .filter_map(|file_match| {
            picker.files.get(file_match.file_idx).map(|path| {
                let path_str = path.to_string_lossy();
                let max_width = layout.body.width.saturating_sub(4) as usize;

                // Handle path truncation with ellipsis
                let (display, adjusted_indices) = if path_str.len() > max_width {
                    // Truncate from the start, keep the end
                    let truncate_at = path_str.len() - max_width + 1;
                    let truncated = format!("…{}", &path_str[truncate_at..]);

                    // Adjust match indices for truncation
                    let adjusted: Vec<usize> = file_match
                        .match_indices
                        .iter()
                        .filter_map(|&idx| {
                            if idx >= truncate_at {
                                // Add 1 for the ellipsis character
                                Some(idx - truncate_at + 1)
                            } else {
                                None // Index was in the truncated portion
                            }
                        })
                        .collect();

                    (truncated, adjusted)
                } else {
                    (path_str.to_string(), file_match.match_indices.clone())
                };

                // Build styled spans with highlighted characters
                let line = build_highlighted_line(&display, &adjusted_indices);
                ListItem::new(line)
            })
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

    render_separator(frame, layout.body, list_height as u16);
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    use super::*;
    use crate::input::CursorMove;
    use crate::overlays::OverlayTransition;

    fn make_key_event(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn create_test_state() -> InputState {
        InputState::new()
    }

    fn apply_input_mutation(input: &mut InputState, mutation: InputMutation) {
        use crate::input::CursorMove;

        match mutation {
            InputMutation::SetTextAndCursor {
                text,
                cursor_row,
                cursor_col,
            } => {
                input.set_text(&text);
                input.textarea.move_cursor(CursorMove::Top);
                input.textarea.move_cursor(CursorMove::Head);
                for _ in 0..cursor_row {
                    input.textarea.move_cursor(CursorMove::Down);
                }
                for _ in 0..cursor_col {
                    input.textarea.move_cursor(CursorMove::Forward);
                }
            }
            InputMutation::SetHistory(history) => {
                input.history = history;
                input.reset_navigation();
            }
            InputMutation::Clear => input.clear(),
            InputMutation::SetText(text) => input.set_text(&text),
            InputMutation::InsertChar(ch) => input.textarea.insert_char(ch),
            InputMutation::ClearHistory => input.clear_history(),
            InputMutation::ClearQueue => input.queued.clear(),
            InputMutation::SetHandoffState(state) => input.handoff = state,
            InputMutation::AttachImage {
                mime_type,
                data,
                source_path,
            } => {
                input.attach_image(mime_type, data, source_path);
            }
            InputMutation::ResetImageCounter => {
                input.reset_image_counter();
            }
        }
    }

    #[test]
    fn test_file_picker_select_file_simple() {
        let mut input = create_test_state();

        input.textarea.insert_str("@");

        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![
            PathBuf::from("crates/zdx-cli/src/main.rs"),
            PathBuf::from("crates/zdx-cli/src/lib.rs"),
        ]);

        let update = picker.handle_key(&input, make_key_event(KeyCode::Enter));
        assert!(matches!(update.transition, OverlayTransition::Close));
        for mutation in update.mutations {
            if let StateMutation::Input(mutation) = mutation {
                apply_input_mutation(&mut input, mutation);
            }
        }

        let text = input.get_text();
        assert_eq!(text, "@crates/zdx-cli/src/main.rs ");
    }

    #[test]
    fn test_file_picker_select_file_with_filter() {
        let mut input = create_test_state();

        input.textarea.insert_str("@lib");

        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![
            PathBuf::from("crates/zdx-cli/src/main.rs"),
            PathBuf::from("crates/zdx-cli/src/lib.rs"),
        ]);
        picker.apply_filter("lib");

        let update = picker.handle_key(&input, make_key_event(KeyCode::Enter));
        assert!(matches!(update.transition, OverlayTransition::Close));
        for mutation in update.mutations {
            if let StateMutation::Input(mutation) = mutation {
                apply_input_mutation(&mut input, mutation);
            }
        }

        let text = input.get_text();
        assert_eq!(text, "@crates/zdx-cli/src/lib.rs ");
    }

    #[test]
    fn test_file_picker_select_with_text_before_and_after() {
        let mut input = create_test_state();

        input.textarea.insert_str("Hello @filter world");
        for _ in 0..6 {
            input.textarea.move_cursor(CursorMove::Back);
        }

        let (mut picker, _) = FilePickerState::open(6);
        picker.set_files(vec![PathBuf::from("crates/zdx-cli/src/main.rs")]);

        let update = picker.handle_key(&input, make_key_event(KeyCode::Tab));
        assert!(matches!(update.transition, OverlayTransition::Close));
        for mutation in update.mutations {
            if let StateMutation::Input(mutation) = mutation {
                apply_input_mutation(&mut input, mutation);
            }
        }

        let text = input.get_text();
        assert_eq!(text, "Hello @crates/zdx-cli/src/main.rs  world");
    }

    #[test]
    fn test_file_picker_select_empty_list_closes() {
        let mut input = create_test_state();

        input.textarea.insert_str("@");

        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![]);

        let update = picker.handle_key(&input, make_key_event(KeyCode::Enter));
        assert!(matches!(update.transition, OverlayTransition::Close));
        for mutation in update.mutations {
            if let StateMutation::Input(mutation) = mutation {
                apply_input_mutation(&mut input, mutation);
            }
        }

        let text = input.get_text();
        assert_eq!(text, "@");
    }

    #[test]
    fn test_file_picker_navigate_then_select() {
        let mut input = create_test_state();

        input.textarea.insert_str("@");

        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![
            PathBuf::from("a.txt"),
            PathBuf::from("b.txt"),
            PathBuf::from("c.txt"),
        ]);

        let _ = picker.handle_key(&input, make_key_event(KeyCode::Down));
        let _ = picker.handle_key(&input, make_key_event(KeyCode::Down));

        let update = picker.handle_key(&input, make_key_event(KeyCode::Enter));
        assert!(matches!(update.transition, OverlayTransition::Close));
        for mutation in update.mutations {
            if let StateMutation::Input(mutation) = mutation {
                apply_input_mutation(&mut input, mutation);
            }
        }

        let text = input.get_text();
        assert_eq!(text, "@c.txt ");
    }

    #[test]
    fn test_fuzzy_matching_basic() {
        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![
            PathBuf::from("crates/zdx-cli/src/main.rs"),
            PathBuf::from("crates/zdx-cli/src/lib.rs"),
            PathBuf::from("crates/zdx-cli/tests/integration.rs"),
        ]);

        // "mr" should match "main.rs" with higher score
        picker.apply_filter("mr");

        assert!(!picker.filtered.is_empty());
        // Should have matches (fuzzy matches "mr" in "main.rs")
        let first_match = &picker.filtered[0];
        let first_path = picker.files.get(first_match.file_idx).unwrap();
        assert_eq!(first_path, &PathBuf::from("crates/zdx-cli/src/main.rs"));
    }

    #[test]
    fn test_fuzzy_matching_scores_better_matches_higher() {
        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![
            PathBuf::from("deeply/nested/config.toml"),
            PathBuf::from("config.toml"),
            PathBuf::from("src/configuration/settings.rs"),
        ]);

        picker.apply_filter("config");

        // "config.toml" should rank higher than deeply nested version
        // because the match is more direct
        assert!(picker.filtered.len() >= 2);

        // All matches should have scores
        for m in &picker.filtered {
            assert!(m.score.is_some());
        }

        // Verify scores are in descending order
        for window in picker.filtered.windows(2) {
            assert!(window[0].score >= window[1].score);
        }
    }

    #[test]
    fn test_fuzzy_matching_captures_indices() {
        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![PathBuf::from("crates/zdx-cli/src/main.rs")]);

        picker.apply_filter("main");

        assert_eq!(picker.filtered.len(), 1);
        let m = &picker.filtered[0];

        // Should have match indices for highlighting
        assert!(!m.match_indices.is_empty());
        // "main" appears at bytes 19-22 in "crates/zdx-cli/src/main.rs"
        assert!(m.match_indices.contains(&19)); // 'm'
        assert!(m.match_indices.contains(&20)); // 'a'
        assert!(m.match_indices.contains(&21)); // 'i'
        assert!(m.match_indices.contains(&22)); // 'n'
    }

    #[test]
    fn test_empty_filter_shows_all_files_without_indices() {
        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![
            PathBuf::from("a.txt"),
            PathBuf::from("b.txt"),
            PathBuf::from("c.txt"),
        ]);

        picker.apply_filter("");

        // All files shown
        assert_eq!(picker.filtered.len(), 3);

        // No highlighting when no filter
        for m in &picker.filtered {
            assert!(m.match_indices.is_empty());
            assert!(m.score.is_none());
        }
    }

    #[test]
    fn test_fuzzy_matching_no_match() {
        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![
            PathBuf::from("crates/zdx-cli/src/main.rs"),
            PathBuf::from("crates/zdx-cli/src/lib.rs"),
        ]);

        picker.apply_filter("xyz123");

        // No matches for nonsense pattern
        assert!(picker.filtered.is_empty());
    }

    #[test]
    fn test_build_highlighted_line_no_matches() {
        let line = build_highlighted_line("crates/zdx-cli/src/main.rs", &[]);
        assert_eq!(line.spans.len(), 1);
        // Should be cyan (non-highlighted)
        assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_build_highlighted_line_with_matches() {
        // Highlight "main" at indices 19-22
        let line = build_highlighted_line("crates/zdx-cli/src/main.rs", &[19, 20, 21, 22]);

        // Should have multiple spans: "src/" (cyan), "main" (yellow), ".rs" (cyan)
        assert!(line.spans.len() >= 3);

        // First span should be cyan (non-matched)
        assert_eq!(line.spans[0].content, "crates/zdx-cli/src/");
        assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));

        // Second span should be yellow+bold (matched)
        assert_eq!(line.spans[1].content, "main");
        assert_eq!(line.spans[1].style.fg, Some(Color::Yellow));
        assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));

        // Third span should be cyan (non-matched)
        assert_eq!(line.spans[2].content, ".rs");
        assert_eq!(line.spans[2].style.fg, Some(Color::Cyan));
    }
}
