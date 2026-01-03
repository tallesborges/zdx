use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use super::OverlayAction;
use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::state::TuiState;

const MAX_VISIBLE_FILES: usize = 10;
const VISIBLE_HEIGHT: usize = MAX_VISIBLE_FILES - 2;
const MAX_FILES: usize = 1000;
const MAX_DEPTH: usize = 15;

#[derive(Debug, Clone)]
pub struct FilePickerState {
    pub trigger_pos: usize,
    pub files: Vec<PathBuf>,
    pub filtered: Vec<usize>,
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
        render_file_picker(frame, self, area, input_y)
    }

    pub fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                Some(OverlayAction::close())
            }
            KeyCode::Enter | KeyCode::Tab => {
                self.select_file_and_insert(tui);
                Some(OverlayAction::close())
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    if self.selected < self.offset {
                        self.offset = self.selected;
                    }
                }
                None
            }
            KeyCode::Down => {
                if self.selected < self.filtered.len().saturating_sub(1) {
                    self.selected += 1;
                    if self.selected >= self.offset + VISIBLE_HEIGHT {
                        self.offset = self.selected - VISIBLE_HEIGHT + 1;
                    }
                }
                None
            }
            KeyCode::Char('p') if ctrl => {
                if self.selected > 0 {
                    self.selected -= 1;
                    if self.selected < self.offset {
                        self.offset = self.selected;
                    }
                }
                None
            }
            KeyCode::Char('n') if ctrl => {
                if self.selected < self.filtered.len().saturating_sub(1) {
                    self.selected += 1;
                    if self.selected >= self.offset + VISIBLE_HEIGHT {
                        self.offset = self.selected - VISIBLE_HEIGHT + 1;
                    }
                }
                None
            }
            _ => {
                tui.input.textarea.input(key);

                let pattern = self.get_filter_pattern(tui);
                self.apply_filter(&pattern);

                if self.is_trigger_deleted(tui) {
                    Some(OverlayAction::close())
                } else {
                    None
                }
            }
        }
    }

    pub fn selected_file(&self) -> Option<&PathBuf> {
        self.filtered
            .get(self.selected)
            .and_then(|&idx| self.files.get(idx))
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

    fn get_cursor_byte_pos(tui: &TuiState) -> usize {
        let text = tui.get_input_text();
        let (row, col) = tui.input.textarea.cursor();
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

    fn get_filter_pattern(&self, tui: &TuiState) -> String {
        let text = tui.get_input_text();
        let trigger_pos = self.trigger_pos;
        let cursor_pos = Self::get_cursor_byte_pos(tui);

        if trigger_pos < text.len() && trigger_pos < cursor_pos {
            let start = trigger_pos + 1;
            let end = cursor_pos.min(text.len());
            if start <= end {
                return text[start..end].to_string();
            }
        }

        String::new()
    }

    fn is_trigger_deleted(&self, tui: &TuiState) -> bool {
        let text = tui.get_input_text();
        let trigger_pos = self.trigger_pos;

        if trigger_pos >= text.len() || text.as_bytes().get(trigger_pos) != Some(&b'@') {
            return true;
        }

        let cursor_pos = Self::get_cursor_byte_pos(tui);
        cursor_pos <= trigger_pos
    }

    fn select_file_and_insert(&self, tui: &mut TuiState) {
        let Some(selected_path) = self.selected_file().cloned() else {
            return;
        };

        let trigger_pos = self.trigger_pos;

        let text = tui.get_input_text();
        let cursor_byte_pos = Self::get_cursor_byte_pos(tui);

        let path_str = selected_path.to_string_lossy();
        let before_at = &text[..=trigger_pos];
        let after_cursor = if cursor_byte_pos < text.len() {
            &text[cursor_byte_pos..]
        } else {
            ""
        };

        let new_text = format!("{}{} {}", before_at, path_str, after_cursor);

        let new_cursor_byte_pos = trigger_pos + 1 + path_str.len() + 1;

        tui.input.textarea.select_all();
        tui.input.textarea.cut();
        tui.input.textarea.insert_str(&new_text);

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

        tui.input
            .textarea
            .move_cursor(tui_textarea::CursorMove::Top);
        tui.input
            .textarea
            .move_cursor(tui_textarea::CursorMove::Head);

        for _ in 0..target_row {
            tui.input
                .textarea
                .move_cursor(tui_textarea::CursorMove::Down);
        }

        for _ in 0..target_col {
            tui.input
                .textarea
                .move_cursor(tui_textarea::CursorMove::Forward);
        }
    }
}

/// Discovers project files, respecting .gitignore.
pub fn discover_files(root: &std::path::Path) -> Vec<PathBuf> {
    use ignore::WalkBuilder;

    let mut files = Vec::new();

    let walker = WalkBuilder::new(root)
        .standard_filters(true)
        .max_depth(Some(MAX_DEPTH))
        .build();

    for entry in walker.flatten() {
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
    let file_count = picker.filtered.len();
    let visible_count = file_count.min(MAX_VISIBLE_FILES);

    let picker_width = 50.min(area.width.saturating_sub(4));
    let base_height = if picker.loading || file_count == 0 {
        5
    } else {
        visible_count as u16 + 4
    };
    let picker_height = base_height.min(area.height / 2);

    let available_height = input_top_y;

    let picker_x = (area.width.saturating_sub(picker_width)) / 2;
    let picker_y = (available_height.saturating_sub(picker_height)) / 2;

    let picker_area = Rect::new(picker_x, picker_y, picker_width, picker_height);

    frame.render_widget(Clear, picker_area);

    let title = if picker.loading {
        " Files (loading...) ".to_string()
    } else {
        format!(" Files ({}) ", file_count)
    };

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
        Span::styled(" nav ", Style::default().fg(Color::DarkGray)),
        Span::styled("Enter", Style::default().fg(Color::Blue)),
        Span::styled(" select ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Blue)),
        Span::styled(" close", Style::default().fg(Color::DarkGray)),
    ]);
    let hints_para = Paragraph::new(hints_line).alignment(Alignment::Center);
    frame.render_widget(hints_para, hints_area);
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    use super::*;
    use crate::config::Config;

    fn make_key_event(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn create_test_state() -> TuiState {
        let config = Config::default();
        TuiState::new(config, std::path::PathBuf::new(), None, None)
    }

    #[test]
    fn test_file_picker_select_file_simple() {
        let mut tui = create_test_state();

        tui.input.textarea.insert_str("@");

        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![
            PathBuf::from("src/main.rs"),
            PathBuf::from("src/lib.rs"),
        ]);

        let action = picker.handle_key(&mut tui, make_key_event(KeyCode::Enter));
        assert!(matches!(action, Some(OverlayAction::Close(_))));

        let text = tui.get_input_text();
        assert_eq!(text, "@src/main.rs ");
    }

    #[test]
    fn test_file_picker_select_file_with_filter() {
        let mut tui = create_test_state();

        tui.input.textarea.insert_str("@lib");

        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![
            PathBuf::from("src/main.rs"),
            PathBuf::from("src/lib.rs"),
        ]);
        picker.apply_filter("lib");

        let action = picker.handle_key(&mut tui, make_key_event(KeyCode::Enter));
        assert!(matches!(action, Some(OverlayAction::Close(_))));

        let text = tui.get_input_text();
        assert_eq!(text, "@src/lib.rs ");
    }

    #[test]
    fn test_file_picker_select_with_text_before_and_after() {
        let mut tui = create_test_state();

        tui.input.textarea.insert_str("Hello @filter world");
        for _ in 0..6 {
            tui.input
                .textarea
                .move_cursor(tui_textarea::CursorMove::Back);
        }

        let (mut picker, _) = FilePickerState::open(6);
        picker.set_files(vec![PathBuf::from("src/main.rs")]);

        let action = picker.handle_key(&mut tui, make_key_event(KeyCode::Tab));
        assert!(matches!(action, Some(OverlayAction::Close(_))));

        let text = tui.get_input_text();
        assert_eq!(text, "Hello @src/main.rs  world");
    }

    #[test]
    fn test_file_picker_select_empty_list_closes() {
        let mut tui = create_test_state();

        tui.input.textarea.insert_str("@");

        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![]);

        let action = picker.handle_key(&mut tui, make_key_event(KeyCode::Enter));
        assert!(matches!(action, Some(OverlayAction::Close(_))));

        let text = tui.get_input_text();
        assert_eq!(text, "@");
    }

    #[test]
    fn test_file_picker_navigate_then_select() {
        let mut tui = create_test_state();

        tui.input.textarea.insert_str("@");

        let (mut picker, _) = FilePickerState::open(0);
        picker.set_files(vec![
            PathBuf::from("a.txt"),
            PathBuf::from("b.txt"),
            PathBuf::from("c.txt"),
        ]);

        picker.handle_key(&mut tui, make_key_event(KeyCode::Down));
        picker.handle_key(&mut tui, make_key_event(KeyCode::Down));

        let action = picker.handle_key(&mut tui, make_key_event(KeyCode::Enter));
        assert!(matches!(action, Some(OverlayAction::Close(_))));

        let text = tui.get_input_text();
        assert_eq!(text, "@c.txt ");
    }
}
