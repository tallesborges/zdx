//! File picker overlay.
//!
//! Contains state, update handlers, and render function for the file picker.
//! This overlay appears when the user types `@` in the input textarea.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::state::{OverlayState, TuiState};

// ============================================================================
// State
// ============================================================================

/// State for the file picker overlay.
#[derive(Debug, Clone)]
pub struct FilePickerState {
    /// Byte position of the `@` trigger character in the input.
    pub trigger_pos: usize,
}

impl FilePickerState {
    /// Creates a new file picker state.
    pub fn new(trigger_pos: usize) -> Self {
        Self { trigger_pos }
    }
}

// ============================================================================
// Update Handlers
// ============================================================================

/// Opens the file picker overlay at the given trigger position.
pub fn open_file_picker(state: &mut TuiState, trigger_pos: usize) {
    if matches!(state.overlay, OverlayState::None) {
        state.overlay = OverlayState::FilePicker(FilePickerState::new(trigger_pos));
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
        _ => {
            // For now, pass other keys through to input
            // This allows reactive filtering as user types after `@`
            state.input.textarea.input(key);
            vec![]
        }
    }
}

// ============================================================================
// Render
// ============================================================================

/// Renders the file picker as an overlay.
pub fn render_file_picker(frame: &mut Frame, _picker: &FilePickerState, area: Rect, input_top_y: u16) {
    let picker_width = 40.min(area.width.saturating_sub(4));
    let picker_height = 10.min(area.height / 2);

    let available_height = input_top_y;

    let picker_x = (area.width.saturating_sub(picker_width)) / 2;
    let picker_y = (available_height.saturating_sub(picker_height)) / 2;

    let picker_area = Rect::new(picker_x, picker_y, picker_width, picker_height);

    frame.render_widget(Clear, picker_area);

    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(" File Picker ")
        .title_style(
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(outer_block, picker_area);

    // Placeholder content
    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    let placeholder = Paragraph::new(vec![
        Line::from(Span::styled(
            "Type to filter files...",
            Style::default().fg(Color::DarkGray),
        )),
        Line::default(),
        Line::from(Span::styled(
            "Esc to close",
            Style::default().fg(Color::DarkGray),
        )),
    ]);
    frame.render_widget(placeholder, inner_area);
}
