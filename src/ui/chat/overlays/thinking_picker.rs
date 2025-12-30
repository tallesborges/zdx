//! Thinking level picker overlay.
//!
//! Contains state, update handlers, and render function for the thinking level picker.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::config::ThinkingLevel;
use crate::ui::chat::effects::UiEffect;
use crate::ui::chat::state::{OverlayState, TuiState};
use crate::ui::transcript::HistoryCell;

// ============================================================================
// State
// ============================================================================

/// State for the thinking level picker overlay.
#[derive(Debug, Clone)]
pub struct ThinkingPickerState {
    /// Currently selected index.
    pub selected: usize,
}

impl ThinkingPickerState {
    /// Creates a new picker state, selecting the current thinking level if found.
    pub fn new(current: ThinkingLevel) -> Self {
        let selected = ThinkingLevel::all()
            .iter()
            .position(|l| *l == current)
            .unwrap_or(0);
        Self { selected }
    }
}

// ============================================================================
// Update Handlers
// ============================================================================

/// Opens the thinking level picker overlay.
pub fn open_thinking_picker(state: &mut TuiState) {
    if matches!(state.overlay, OverlayState::None) {
        state.overlay =
            OverlayState::ThinkingPicker(ThinkingPickerState::new(state.config.thinking_level));
    }
}

/// Closes the thinking level picker overlay.
pub fn close_thinking_picker(state: &mut TuiState) {
    state.overlay = OverlayState::None;
}

/// Handles key events for the thinking level picker.
pub fn handle_thinking_picker_key(
    state: &mut TuiState,
    key: crossterm::event::KeyEvent,
) -> Vec<UiEffect> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        KeyCode::Esc => {
            close_thinking_picker(state);
            vec![]
        }
        KeyCode::Char('c') if ctrl => {
            close_thinking_picker(state);
            vec![]
        }
        KeyCode::Up => {
            thinking_picker_select_prev(state);
            vec![]
        }
        KeyCode::Down => {
            thinking_picker_select_next(state);
            vec![]
        }
        KeyCode::Enter => execute_thinking_selection(state),
        _ => vec![],
    }
}

fn thinking_picker_select_prev(state: &mut TuiState) {
    if let Some(picker) = state.overlay.as_thinking_picker_mut()
        && picker.selected > 0
    {
        picker.selected -= 1;
    }
}

fn thinking_picker_select_next(state: &mut TuiState) {
    if let Some(picker) = state.overlay.as_thinking_picker_mut()
        && picker.selected < ThinkingLevel::all().len() - 1
    {
        picker.selected += 1;
    }
}

fn execute_thinking_selection(state: &mut TuiState) -> Vec<UiEffect> {
    let Some(picker) = state.overlay.as_thinking_picker() else {
        return vec![];
    };

    let levels = ThinkingLevel::all();
    let Some(&level) = levels.get(picker.selected) else {
        close_thinking_picker(state);
        return vec![];
    };

    // Update state
    state.config.thinking_level = level;
    close_thinking_picker(state);

    // Show confirmation message
    let message = if level == ThinkingLevel::Off {
        "Thinking disabled".to_string()
    } else {
        format!("Thinking level set to {}", level.display_name())
    };
    state.transcript.cells.push(HistoryCell::system(message));

    vec![UiEffect::PersistThinking { level }]
}

// ============================================================================
// Render
// ============================================================================

/// Renders the thinking level picker as an overlay.
pub fn render_thinking_picker(
    frame: &mut Frame,
    picker: &ThinkingPickerState,
    area: Rect,
    input_top_y: u16,
) {
    let levels = ThinkingLevel::all();

    // Calculate picker dimensions
    // Width: enough for level name + description
    let picker_width = 45.min(area.width.saturating_sub(4));
    let picker_height = (levels.len() as u16 + 5).min(area.height / 2);

    let available_height = input_top_y;

    let picker_x = (area.width.saturating_sub(picker_width)) / 2;
    let picker_y = (available_height.saturating_sub(picker_height)) / 2;

    let picker_area = Rect::new(picker_x, picker_y, picker_width, picker_height);

    frame.render_widget(Clear, picker_area);

    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(" Thinking Level ")
        .title_style(
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(outer_block, picker_area);

    let inner_area = Rect::new(
        picker_area.x + 1,
        picker_area.y + 1,
        picker_area.width.saturating_sub(2),
        picker_area.height.saturating_sub(2),
    );

    let list_height = inner_area.height.saturating_sub(2);
    let list_area = Rect::new(inner_area.x, inner_area.y, inner_area.width, list_height);

    let items: Vec<ListItem> = levels
        .iter()
        .map(|level| {
            let name_width = 10; // Fixed width for level name column
            let name = format!("{:<width$}", level.display_name(), width = name_width);

            let line = Line::from(vec![
                Span::styled(
                    name,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(level.description(), Style::default().fg(Color::DarkGray)),
            ]);
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
    list_state.select(Some(picker.selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    // Separator line
    let separator = "─".repeat(inner_area.width as usize);
    let sep_y = inner_area.y + list_height;
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
        Span::styled("↑↓", Style::default().fg(Color::Magenta)),
        Span::styled(" navigate ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Enter", Style::default().fg(Color::Magenta)),
        Span::styled(" select ", Style::default().fg(Color::DarkGray)),
        Span::styled("•", Style::default().fg(Color::DarkGray)),
        Span::styled(" Esc", Style::default().fg(Color::Magenta)),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ]);
    let hints_para = Paragraph::new(hints_line).alignment(Alignment::Center);
    frame.render_widget(hints_para, hints_area);
}
