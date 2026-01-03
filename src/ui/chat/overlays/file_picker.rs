//! File picker overlay.
//!
//! Contains state, update handlers, and render function for the file picker.
//! This overlay appears when the user types `@` in the input textarea.
//!
//! ## Architecture Note
//!
//! Overlay handlers mutate `TuiState` directly through the `handle_key` trait method.
//! The split state architecture allows clean access to both `&mut self` (the overlay)
//! and `&mut TuiState` without borrow conflicts.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use super::{Overlay, OverlayAction, OverlayState};
use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::state::TuiState;

// ============================================================================
// Constants
// ============================================================================

/// Maximum visible files in the picker (excluding borders and hints).
const MAX_VISIBLE_FILES: usize = 10;

/// Visible height used for scroll offset calculations.
const VISIBLE_HEIGHT: usize = MAX_VISIBLE_FILES - 2;

/// Maximum number of files to discover (performance limit).
const MAX_FILES: usize = 1000;

/// Maximum directory depth to walk.
const MAX_DEPTH: usize = 15;

// ============================================================================
// State
// ============================================================================

/// State for the file picker overlay.
#[derive(Debug, Clone)]
pub struct FilePickerState {
    /// Byte position of the `@` trigger character in the input.
    pub trigger_pos: usize,
    /// All discovered files (relative paths from project root).
    pub files: Vec<PathBuf>,
    /// Indices into `files` that match the current filter.
    pub filtered: Vec<usize>,
    /// Currently selected index in the filtered list.
    pub selected: usize,
    /// Scroll offset for long lists.
    pub offset: usize,
    /// Whether files are still being loaded.
    pub loading: bool,
}

impl FilePickerState {
    /// Creates a new file picker state.
    pub fn new(trigger_pos: usize) -> Self {
        Self {
            trigger_pos,
            files: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            offset: 0,
            loading: true,
        }
    }

    /// Returns the currently selected file path, if any.
    pub fn selected_file(&self) -> Option<&PathBuf> {
        self.filtered
            .get(self.selected)
            .and_then(|&idx| self.files.get(idx))
    }

    /// Updates the filtered list based on a filter pattern.
    ///
    /// Uses case-insensitive substring matching.
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

        // Reset selection when filter changes
        self.selected = 0;
        self.offset = 0;
    }

    /// Sets the file list and marks loading as complete.
    pub fn set_files(&mut self, files: Vec<PathBuf>) {
        self.files = files;
        self.loading = false;
        // Initialize filtered to all files
        self.filtered = (0..self.files.len()).collect();
    }
}

// ============================================================================
// Overlay Trait Implementation
// ============================================================================

impl Overlay for FilePickerState {
    fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_file_picker(frame, self, area, input_y)
    }

    fn handle_key(&mut self, tui: &mut TuiState, key: KeyEvent) -> Option<OverlayAction> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                // Close picker but leave `@` in input
                Some(OverlayAction::close())
            }
            KeyCode::Enter | KeyCode::Tab => {
                // Select current file and insert into input
                self.select_file_and_insert(tui);
                Some(OverlayAction::close())
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    // Adjust offset to keep selection visible
                    if self.selected < self.offset {
                        self.offset = self.selected;
                    }
                }
                None
            }
            KeyCode::Down => {
                if self.selected < self.filtered.len().saturating_sub(1) {
                    self.selected += 1;
                    // Adjust offset to keep selection visible
                    if self.selected >= self.offset + VISIBLE_HEIGHT {
                        self.offset = self.selected - VISIBLE_HEIGHT + 1;
                    }
                }
                None
            }
            KeyCode::Char('p') if ctrl => {
                // Ctrl+P also navigates up (vim-like)
                if self.selected > 0 {
                    self.selected -= 1;
                    if self.selected < self.offset {
                        self.offset = self.selected;
                    }
                }
                None
            }
            KeyCode::Char('n') if ctrl => {
                // Ctrl+N also navigates down (vim-like)
                if self.selected < self.filtered.len().saturating_sub(1) {
                    self.selected += 1;
                    if self.selected >= self.offset + VISIBLE_HEIGHT {
                        self.offset = self.selected - VISIBLE_HEIGHT + 1;
                    }
                }
                None
            }
            _ => {
                // Pass other keys through to input for reactive filtering
                tui.input.textarea.input(key);

                // Update filter based on new input text
                let pattern = self.get_filter_pattern(tui);
                self.apply_filter(&pattern);

                // Check if `@` was deleted (backspace past trigger)
                if self.is_trigger_deleted(tui) {
                    Some(OverlayAction::close())
                } else {
                    None
                }
            }
        }
    }
}

impl FilePickerState {
    /// Calculates the cursor byte position in the input text.
    fn get_cursor_byte_pos(tui: &TuiState) -> usize {
        let text = tui.get_input_text();
        let (row, col) = tui.input.textarea.cursor();
        let lines: Vec<&str> = text.lines().collect();

        let mut pos = 0;
        for (i, line) in lines.iter().enumerate() {
            if i < row {
                pos += line.len() + 1; // +1 for newline
            } else {
                pos += col;
                break;
            }
        }
        pos
    }

    /// Extracts the filter pattern from input (text after `@` up to cursor).
    fn get_filter_pattern(&self, tui: &TuiState) -> String {
        let text = tui.get_input_text();
        let trigger_pos = self.trigger_pos;
        let cursor_pos = Self::get_cursor_byte_pos(tui);

        // Extract text between `@` (exclusive) and cursor
        if trigger_pos < text.len() && trigger_pos < cursor_pos {
            // Skip the `@` character itself
            let start = trigger_pos + 1;
            let end = cursor_pos.min(text.len());
            if start <= end {
                return text[start..end].to_string();
            }
        }

        String::new()
    }

    /// Checks if the `@` trigger character was deleted.
    fn is_trigger_deleted(&self, tui: &TuiState) -> bool {
        let text = tui.get_input_text();
        let trigger_pos = self.trigger_pos;

        // If trigger position is beyond text length, or the character at trigger_pos is not `@`
        if trigger_pos >= text.len() || text.as_bytes().get(trigger_pos) != Some(&b'@') {
            return true;
        }

        // Also close if cursor moved before the trigger position
        let cursor_pos = Self::get_cursor_byte_pos(tui);
        cursor_pos <= trigger_pos
    }

    /// Selects the current file and inserts it into the input textarea.
    ///
    /// Replaces `@<filter>` with `@<selected-file-path> ` (with trailing space).
    /// Positions cursor after the inserted path.
    fn select_file_and_insert(&self, tui: &mut TuiState) {
        let Some(selected_path) = self.selected_file().cloned() else {
            // No file selected (empty list), just close
            return;
        };

        let trigger_pos = self.trigger_pos;

        // Get current text and cursor byte position
        let text = tui.get_input_text();
        let cursor_byte_pos = Self::get_cursor_byte_pos(tui);

        // Build the new text:
        // - Keep everything up to and including `@` (trigger_pos + 1)
        // - Insert file path + trailing space
        // - Keep everything after cursor
        let path_str = selected_path.to_string_lossy();
        let before_at = &text[..=trigger_pos]; // includes the `@`
        let after_cursor = if cursor_byte_pos < text.len() {
            &text[cursor_byte_pos..]
        } else {
            ""
        };

        let new_text = format!("{}{} {}", before_at, path_str, after_cursor);

        // Calculate new cursor position (after path + space)
        // New cursor = trigger_pos + 1 (@) + path_len + 1 (space)
        let new_cursor_byte_pos = trigger_pos + 1 + path_str.len() + 1;

        // Set the new text
        tui.input.textarea.select_all();
        tui.input.textarea.cut();
        tui.input.textarea.insert_str(&new_text);

        // Position cursor at the correct location
        // Convert byte position back to (row, col)
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
            remaining -= line.len() + 1; // +1 for newline
            target_row = i + 1;
            target_col = 0;
        }

        // Handle case where cursor is at the very end
        if target_row >= new_lines.len() {
            target_row = new_lines.len().saturating_sub(1);
            target_col = new_lines.last().map(|l| l.len()).unwrap_or(0);
        }

        // Move cursor to target position
        // First move to start, then to the target row/col
        tui.input
            .textarea
            .move_cursor(tui_textarea::CursorMove::Top);
        tui.input
            .textarea
            .move_cursor(tui_textarea::CursorMove::Head);

        // Move down to target row
        for _ in 0..target_row {
            tui.input
                .textarea
                .move_cursor(tui_textarea::CursorMove::Down);
        }

        // Move to target column
        for _ in 0..target_col {
            tui.input
                .textarea
                .move_cursor(tui_textarea::CursorMove::Forward);
        }
    }
}

// ============================================================================
// Update Handlers
// ============================================================================

/// Opens the file picker overlay at the given trigger position.
pub fn open_file_picker(overlay: &mut OverlayState, trigger_pos: usize) -> Vec<UiEffect> {
    if matches!(overlay, OverlayState::None) {
        *overlay = OverlayState::FilePicker(FilePickerState::new(trigger_pos));
        // Return effect to discover files asynchronously
        vec![UiEffect::DiscoverFiles]
    } else {
        vec![]
    }
}

// ============================================================================
// File Discovery
// ============================================================================

/// Discovers project files, respecting .gitignore.
///
/// Returns relative paths sorted alphabetically.
/// Hidden files (starting with `.`) are excluded.
pub fn discover_files(root: &std::path::Path) -> Vec<PathBuf> {
    use ignore::WalkBuilder;

    let mut files = Vec::new();

    let walker = WalkBuilder::new(root)
        .standard_filters(true)
        .max_depth(Some(MAX_DEPTH))
        .build();

    for entry in walker.flatten() {
        // Skip directories, only include files
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        // Get path relative to root
        if let Ok(rel_path) = entry.path().strip_prefix(root) {
            // Skip empty paths (the root itself)
            if rel_path.as_os_str().is_empty() {
                continue;
            }

            files.push(rel_path.to_path_buf());

            // Limit file count for performance
            if files.len() >= MAX_FILES {
                break;
            }
        }
    }

    // Sort alphabetically for consistent display
    files.sort();

    files
}

// ============================================================================
// Render
// ============================================================================

/// Renders the file picker as an overlay.
pub fn render_file_picker(
    frame: &mut Frame,
    picker: &FilePickerState,
    area: Rect,
    input_top_y: u16,
) {
    // Calculate dimensions
    let file_count = picker.filtered.len();
    let visible_count = file_count.min(MAX_VISIBLE_FILES);

    // Width: enough for typical file paths
    let picker_width = 50.min(area.width.saturating_sub(4));
    // Height: visible files + border (2) + hints (2)
    let base_height = if picker.loading || file_count == 0 {
        5 // Minimal height for loading/empty state
    } else {
        visible_count as u16 + 4
    };
    let picker_height = base_height.min(area.height / 2);

    let available_height = input_top_y;

    let picker_x = (area.width.saturating_sub(picker_width)) / 2;
    let picker_y = (available_height.saturating_sub(picker_height)) / 2;

    let picker_area = Rect::new(picker_x, picker_y, picker_width, picker_height);

    frame.render_widget(Clear, picker_area);

    // Title with file count
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

    // Handle loading state
    if picker.loading {
        let loading_msg = Paragraph::new("Loading files...")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        frame.render_widget(loading_msg, inner_area);
        return;
    }

    // Handle empty state
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

    // Calculate list area (leaving room for hints)
    let list_height = inner_area.height.saturating_sub(2) as usize;
    let list_area = Rect::new(
        inner_area.x,
        inner_area.y,
        inner_area.width,
        list_height as u16,
    );

    // Build list items for visible files
    let items: Vec<ListItem> = picker
        .filtered
        .iter()
        .skip(picker.offset)
        .take(list_height)
        .filter_map(|&idx| picker.files.get(idx))
        .map(|path| {
            // Truncate long paths with ellipsis at the start
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

    // Adjust selected index for the visible window
    let mut list_state = ListState::default();
    let visible_selected = picker.selected.saturating_sub(picker.offset);
    list_state.select(Some(visible_selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    // Separator line
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

    // Keyboard hints
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
        let mut overlay = OverlayState::None;

        // Type "@" in input
        tui.input.textarea.insert_str("@");

        // Open file picker at position 0 (where @ is)
        open_file_picker(&mut overlay, 0);

        // Set some files
        if let OverlayState::FilePicker(ref mut picker) = overlay {
            picker.set_files(vec![
                PathBuf::from("src/main.rs"),
                PathBuf::from("src/lib.rs"),
            ]);

            // Use handle_key with Enter
            let action = picker.handle_key(&mut tui, make_key_event(KeyCode::Enter));
            assert!(matches!(action, Some(OverlayAction::Close(_))));
        }

        // Verify input contains the selected file
        let text = tui.get_input_text();
        assert_eq!(text, "@src/main.rs ");
    }

    #[test]
    fn test_file_picker_select_file_with_filter() {
        let mut tui = create_test_state();
        let mut overlay = OverlayState::None;

        // Type "@lib" in input (@ at position 0, cursor at position 4)
        tui.input.textarea.insert_str("@lib");

        // Open file picker at position 0 (where @ is)
        open_file_picker(&mut overlay, 0);

        // Set some files
        if let OverlayState::FilePicker(ref mut picker) = overlay {
            picker.set_files(vec![
                PathBuf::from("src/main.rs"),
                PathBuf::from("src/lib.rs"),
            ]);
            // Apply filter for "lib"
            picker.apply_filter("lib");

            // Use handle_key with Enter
            let action = picker.handle_key(&mut tui, make_key_event(KeyCode::Enter));
            assert!(matches!(action, Some(OverlayAction::Close(_))));
        }

        // Verify input contains the selected file (filter "lib" replaced with path)
        let text = tui.get_input_text();
        assert_eq!(text, "@src/lib.rs ");
    }

    #[test]
    fn test_file_picker_select_with_text_before_and_after() {
        let mut tui = create_test_state();
        let mut overlay = OverlayState::None;

        // Type "Hello @filter world" in input (@ at position 6)
        tui.input.textarea.insert_str("Hello @filter world");
        // Move cursor back to after "filter" (position 13)
        for _ in 0..6 {
            // Move back 6 characters (" world")
            tui.input
                .textarea
                .move_cursor(tui_textarea::CursorMove::Back);
        }

        // Open file picker at position 6 (where @ is)
        open_file_picker(&mut overlay, 6);

        // Set some files
        if let OverlayState::FilePicker(ref mut picker) = overlay {
            picker.set_files(vec![PathBuf::from("src/main.rs")]);

            // Use handle_key with Tab (same as Enter)
            let action = picker.handle_key(&mut tui, make_key_event(KeyCode::Tab));
            assert!(matches!(action, Some(OverlayAction::Close(_))));
        }

        // Verify input contains the selected file with surrounding text preserved
        let text = tui.get_input_text();
        assert_eq!(text, "Hello @src/main.rs  world");
    }

    #[test]
    fn test_file_picker_select_empty_list_closes() {
        let mut tui = create_test_state();
        let mut overlay = OverlayState::None;

        // Type "@" in input
        tui.input.textarea.insert_str("@");

        // Open file picker
        open_file_picker(&mut overlay, 0);

        // Set empty file list
        if let OverlayState::FilePicker(ref mut picker) = overlay {
            picker.set_files(vec![]);

            // Press Enter (should just close, no crash)
            let action = picker.handle_key(&mut tui, make_key_event(KeyCode::Enter));
            assert!(matches!(action, Some(OverlayAction::Close(_))));
        }

        // Input should still have just "@"
        let text = tui.get_input_text();
        assert_eq!(text, "@");
    }

    #[test]
    fn test_file_picker_navigate_then_select() {
        let mut tui = create_test_state();
        let mut overlay = OverlayState::None;

        // Type "@" in input
        tui.input.textarea.insert_str("@");

        // Open file picker
        open_file_picker(&mut overlay, 0);

        // Set files
        if let OverlayState::FilePicker(ref mut picker) = overlay {
            picker.set_files(vec![
                PathBuf::from("a.txt"),
                PathBuf::from("b.txt"),
                PathBuf::from("c.txt"),
            ]);

            // Navigate down twice to select "c.txt"
            picker.handle_key(&mut tui, make_key_event(KeyCode::Down));
            picker.handle_key(&mut tui, make_key_event(KeyCode::Down));

            // Press Enter to select
            let action = picker.handle_key(&mut tui, make_key_event(KeyCode::Enter));
            assert!(matches!(action, Some(OverlayAction::Close(_))));
        }

        // Verify the third file was selected
        let text = tui.get_input_text();
        assert_eq!(text, "@c.txt ");
    }
}
