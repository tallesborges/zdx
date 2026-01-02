//! File picker overlay.
//!
//! Contains state, update handlers, and render function for the file picker.
//! This overlay appears when the user types `@` in the input textarea.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::state::{OverlayState, TuiState};

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
// Update Handlers
// ============================================================================

/// Opens the file picker overlay at the given trigger position.
pub fn open_file_picker(state: &mut TuiState, trigger_pos: usize) -> Vec<UiEffect> {
    if matches!(state.overlay, OverlayState::None) {
        state.overlay = OverlayState::FilePicker(FilePickerState::new(trigger_pos));
        // Return effect to discover files asynchronously
        vec![UiEffect::DiscoverFiles]
    } else {
        vec![]
    }
}

/// Closes the file picker overlay.
pub fn close_file_picker(state: &mut TuiState) {
    state.overlay = OverlayState::None;
}

/// Handles key events for the file picker.
pub fn handle_file_picker_key(
    state: &mut TuiState,
    key: crossterm::event::KeyEvent,
) -> Vec<UiEffect> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        KeyCode::Esc => {
            // Close picker but leave `@` in input
            close_file_picker(state);
            vec![]
        }
        KeyCode::Char('c') if ctrl => {
            // Ctrl+C also closes
            close_file_picker(state);
            vec![]
        }
        KeyCode::Up => {
            file_picker_select_prev(state);
            vec![]
        }
        KeyCode::Down => {
            file_picker_select_next(state);
            vec![]
        }
        KeyCode::Char('p') if ctrl => {
            // Ctrl+P also navigates up (vim-like)
            file_picker_select_prev(state);
            vec![]
        }
        KeyCode::Char('n') if ctrl => {
            // Ctrl+N also navigates down (vim-like)
            file_picker_select_next(state);
            vec![]
        }
        _ => {
            // Pass other keys through to input for reactive filtering
            state.input.textarea.input(key);

            // Update filter based on new input text
            update_filter_from_input(state);

            // Check if `@` was deleted (backspace past trigger)
            check_trigger_deleted(state);

            vec![]
        }
    }
}

/// Extracts the filter pattern from input (text after `@` up to cursor).
fn get_filter_pattern(state: &TuiState) -> String {
    let Some(picker) = state.overlay.as_file_picker() else {
        return String::new();
    };

    let text = state.get_input_text();
    let trigger_pos = picker.trigger_pos;

    // Calculate cursor byte position
    let (row, col) = state.input.textarea.cursor();
    let lines: Vec<&str> = text.lines().collect();
    let mut cursor_pos = 0;
    for (i, line) in lines.iter().enumerate() {
        if i < row {
            cursor_pos += line.len() + 1; // +1 for newline
        } else {
            cursor_pos += col;
            break;
        }
    }

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

/// Updates the filter based on current input text.
fn update_filter_from_input(state: &mut TuiState) {
    let pattern = get_filter_pattern(state);

    if let Some(picker) = state.overlay.as_file_picker_mut() {
        picker.apply_filter(&pattern);
    }
}

/// Checks if the `@` trigger character was deleted.
fn check_trigger_deleted(state: &mut TuiState) {
    let Some(picker) = state.overlay.as_file_picker() else {
        return;
    };

    let text = state.get_input_text();
    let trigger_pos = picker.trigger_pos;

    // If trigger position is beyond text length, or the character at trigger_pos is not `@`
    if trigger_pos >= text.len() || text.as_bytes().get(trigger_pos) != Some(&b'@') {
        close_file_picker(state);
        return;
    }

    // Also close if cursor moved before the trigger position
    let (row, col) = state.input.textarea.cursor();
    let lines: Vec<&str> = text.lines().collect();
    let mut cursor_pos = 0;
    for (i, line) in lines.iter().enumerate() {
        if i < row {
            cursor_pos += line.len() + 1;
        } else {
            cursor_pos += col;
            break;
        }
    }

    if cursor_pos <= trigger_pos {
        close_file_picker(state);
    }
}

fn file_picker_select_prev(state: &mut TuiState) {
    if let Some(picker) = state.overlay.as_file_picker_mut()
        && picker.selected > 0
    {
        picker.selected -= 1;
        // Adjust offset to keep selection visible
        if picker.selected < picker.offset {
            picker.offset = picker.selected;
        }
    }
}

fn file_picker_select_next(state: &mut TuiState) {
    if let Some(picker) = state.overlay.as_file_picker_mut()
        && picker.selected < picker.filtered.len().saturating_sub(1)
    {
        picker.selected += 1;
        // Adjust offset to keep selection visible
        if picker.selected >= picker.offset + VISIBLE_HEIGHT {
            picker.offset = picker.selected - VISIBLE_HEIGHT + 1;
        }
    }
}

// ============================================================================
// File Discovery
// ============================================================================

/// Discovers project files, respecting .gitignore.
///
/// Returns relative paths sorted alphabetically.
pub fn discover_files(root: &std::path::Path) -> Vec<PathBuf> {
    use ignore::WalkBuilder;

    let mut files = Vec::new();

    let walker = WalkBuilder::new(root)
        .hidden(false) // Show hidden files
        .git_ignore(true) // Respect .gitignore
        .git_global(true) // Respect global gitignore
        .git_exclude(true) // Respect .git/info/exclude
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
        Span::styled(" navigate ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" type", Style::default().fg(Color::Blue)),
        Span::styled(" filter ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Esc", Style::default().fg(Color::Blue)),
        Span::styled(" close", Style::default().fg(Color::DarkGray)),
    ]);
    let hints_para = Paragraph::new(hints_line).alignment(Alignment::Center);
    frame.render_widget(hints_para, hints_area);
}
