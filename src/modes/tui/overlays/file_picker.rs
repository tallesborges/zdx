use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use tokio_util::sync::CancellationToken;

use super::OverlayUpdate;
use crate::modes::tui::input::InputState;
use crate::modes::tui::shared::effects::UiEffect;
use crate::modes::tui::shared::internal::{InputMutation, StateMutation};

const MAX_VISIBLE_FILES: usize = 10;
const VISIBLE_HEIGHT: usize = MAX_VISIBLE_FILES - 2;
const MAX_FILES: usize = 1000;
const MAX_DEPTH: usize = 15;

/// File picker state.
///
/// With the inbox pattern, file discovery results arrive via the inbox.
/// The `discovery_cancel` token is used to cancel the background file walk.
#[derive(Debug)]
pub struct FilePickerState {
    pub trigger_pos: usize,
    pub files: Vec<PathBuf>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub offset: usize,
    pub loading: bool,
    /// Token to cancel the background file walk.
    ///
    /// Stored when `FileDiscoveryStarted` arrives. Cancelled on Drop or via
    /// `UiEffect::CancelFileDiscovery`.
    pub discovery_cancel: Option<CancellationToken>,
}

impl Drop for FilePickerState {
    fn drop(&mut self) {
        if let Some(cancel) = &self.discovery_cancel {
            cancel.cancel();
        }
    }
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
                discovery_cancel: None,
            },
            vec![UiEffect::DiscoverFiles],
        )
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_file_picker(frame, self, area, input_y)
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
            .and_then(|&idx| self.files.get(idx))
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
        let pattern_lower = pattern.to_lowercase();

        self.filtered = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, path)| {
                if pattern.is_empty() {
                    true
                } else {
                    path.to_string_lossy()
                        .to_lowercase()
                        .contains(&pattern_lower)
                }
            })
            .map(|(idx, _)| idx)
            .collect();

        self.selected = 0;
        self.offset = 0;
    }

    pub fn set_files(&mut self, files: Vec<PathBuf>) {
        self.files = files;
        self.loading = false;
        self.filtered = (0..self.files.len()).collect();
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

        let new_text = format!("{}{} {}", before_at, path_str, after_cursor);

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
            target_col = new_lines.last().map(|l| l.len()).unwrap_or(0);
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

            if files.len() >= MAX_FILES {
                break;
            }
        }
    }

    files.sort();
    files
}

pub fn render_file_picker(
    frame: &mut Frame,
    picker: &FilePickerState,
    area: Rect,
    input_top_y: u16,
) {
    use super::render_utils::{
        InputHint, calculate_overlay_area, render_hints, render_overlay_container, render_separator,
    };

    let file_count = picker.filtered.len();
    let visible_count = file_count.min(MAX_VISIBLE_FILES);

    let picker_width = 50;
    let base_height = if picker.loading || file_count == 0 {
        5
    } else {
        visible_count as u16 + 4
    };
    let picker_height = base_height.max(7);

    let picker_area = calculate_overlay_area(area, input_top_y, picker_width, picker_height);
    let title = if picker.loading {
        "Files (loading...)".to_string()
    } else {
        format!("Files ({})", file_count)
    };
    render_overlay_container(frame, picker_area, &title, Color::Blue);

    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    if picker.loading {
        let loading_msg = Paragraph::new("Loading files...")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        frame.render_widget(loading_msg, inner_area);
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

    let items: Vec<ListItem> = picker
        .filtered
        .iter()
        .skip(picker.offset)
        .take(list_height)
        .filter_map(|&idx| picker.files.get(idx))
        .map(|path| {
            let path_str = path.to_string_lossy();
            let max_width = inner_area.width.saturating_sub(4) as usize;
            let display = if path_str.len() > max_width {
                format!("…{}", &path_str[path_str.len() - max_width + 1..])
            } else {
                path_str.to_string()
            };

            let line = Line::from(Span::styled(display, Style::default().fg(Color::Cyan)));
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

    render_separator(frame, inner_area, list_height as u16);

    render_hints(
        frame,
        inner_area,
        &[
            InputHint::new("↑↓", "nav"),
            InputHint::new("Enter", "select"),
            InputHint::new("Esc", "close"),
        ],
        Color::Blue,
    );
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    use super::*;
    use crate::modes::tui::overlays::OverlayTransition;

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
        use tui_textarea::CursorMove;

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
        }
    }

    #[test]
    fn test_file_picker_select_file_simple() {
        let mut input = create_test_state();

        input.textarea.insert_str("@");

        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![
            PathBuf::from("src/main.rs"),
            PathBuf::from("src/lib.rs"),
        ]);

        let update = picker.handle_key(&input, make_key_event(KeyCode::Enter));
        assert!(matches!(update.transition, OverlayTransition::Close));
        for mutation in update.mutations {
            if let StateMutation::Input(mutation) = mutation {
                apply_input_mutation(&mut input, mutation);
            }
        }

        let text = input.get_text();
        assert_eq!(text, "@src/main.rs ");
    }

    #[test]
    fn test_file_picker_select_file_with_filter() {
        let mut input = create_test_state();

        input.textarea.insert_str("@lib");

        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![
            PathBuf::from("src/main.rs"),
            PathBuf::from("src/lib.rs"),
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
        assert_eq!(text, "@src/lib.rs ");
    }

    #[test]
    fn test_file_picker_select_with_text_before_and_after() {
        let mut input = create_test_state();

        input.textarea.insert_str("Hello @filter world");
        for _ in 0..6 {
            input.textarea.move_cursor(tui_textarea::CursorMove::Back);
        }

        let (mut picker, _) = FilePickerState::open(6);
        picker.set_files(vec![PathBuf::from("src/main.rs")]);

        let update = picker.handle_key(&input, make_key_event(KeyCode::Tab));
        assert!(matches!(update.transition, OverlayTransition::Close));
        for mutation in update.mutations {
            if let StateMutation::Input(mutation) = mutation {
                apply_input_mutation(&mut input, mutation);
            }
        }

        let text = input.get_text();
        assert_eq!(text, "Hello @src/main.rs  world");
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
}
