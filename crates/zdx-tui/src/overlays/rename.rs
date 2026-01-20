//! Rename overlay for thread renaming.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::OverlayUpdate;
use crate::common::truncate_start_with_ellipsis;
use crate::effects::UiEffect;
use crate::state::TuiState;

/// State for the rename overlay.
#[derive(Debug, Clone)]
pub struct RenameState {
    /// The current input text for the new title.
    pub input: String,
    /// The thread ID being renamed.
    pub thread_id: String,
    /// Current title (shown as placeholder if input is empty).
    pub current_title: Option<String>,
    /// Error message to display (e.g., empty title).
    pub error: Option<String>,
}

impl RenameState {
    /// Opens the rename overlay for a given thread.
    pub fn open(thread_id: String, current_title: Option<String>) -> (Self, Vec<UiEffect>) {
        (
            Self {
                input: String::new(),
                thread_id,
                current_title,
                error: None,
            },
            vec![],
        )
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_rename_overlay(frame, self, area, input_y)
    }

    pub fn handle_key(&mut self, tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Clear error on any input
        if !matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
            self.error = None;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                OverlayUpdate::close()
            }
            KeyCode::Enter => {
                let title = self.input.trim();
                if title.is_empty() {
                    // Empty title - show error but stay open
                    self.error = Some("Title cannot be empty".to_string());
                    OverlayUpdate::stay()
                } else if tui.tasks.thread_rename.is_running() {
                    // Already renaming - show feedback
                    self.error = Some("Rename in progress...".to_string());
                    OverlayUpdate::stay()
                } else {
                    // Submit the rename
                    OverlayUpdate::close().with_ui_effects(vec![UiEffect::RenameThread {
                        task: None,
                        thread_id: self.thread_id.clone(),
                        title: Some(title.to_string()),
                    }])
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
                OverlayUpdate::stay()
            }
            KeyCode::Char(c) if !ctrl => {
                self.input.push(c);
                OverlayUpdate::stay()
            }
            _ => OverlayUpdate::stay(),
        }
    }
}

fn render_rename_overlay(frame: &mut Frame, state: &RenameState, area: Rect, input_top_y: u16) {
    use super::render_utils::{
        InputHint, calculate_overlay_area, render_hints, render_overlay_container, render_separator,
    };

    let overlay_width = 50;
    let overlay_height = 7;

    let overlay_area = calculate_overlay_area(area, input_top_y, overlay_width, overlay_height);
    render_overlay_container(frame, overlay_area, "Rename Thread", Color::Yellow);

    let inner_area = Rect::new(
        overlay_area.x + 1,
        overlay_area.y + 1,
        overlay_area.width.saturating_sub(2),
        overlay_area.height.saturating_sub(2),
    );

    // Input line with unicode-safe truncation
    let max_input_width = inner_area.width.saturating_sub(4) as usize;
    let (display_text, text_style) = if state.input.is_empty() {
        // Show placeholder (current title or hint)
        let placeholder = state
            .current_title
            .as_deref()
            .unwrap_or("Enter new title...");
        (
            placeholder.to_string(),
            Style::default().fg(Color::DarkGray),
        )
    } else {
        // Use unicode-safe truncation from the start
        let truncated = truncate_start_with_ellipsis(&state.input, max_input_width);
        (truncated, Style::default().fg(Color::Yellow))
    };

    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::styled(&display_text, text_style),
        Span::styled("█", Style::default().fg(Color::Yellow)),
    ]);
    let input_para = Paragraph::new(input_line);
    let input_area = Rect::new(inner_area.x, inner_area.y, inner_area.width, 1);
    frame.render_widget(input_para, input_area);

    render_separator(frame, inner_area, 1);

    // Help text or error message
    let (help_text, help_style) = if let Some(error) = &state.error {
        (error.as_str(), Style::default().fg(Color::Red))
    } else if state.current_title.is_some() {
        (
            "Type a new title for this thread",
            Style::default().fg(Color::DarkGray),
        )
    } else {
        (
            "Type a title for this thread",
            Style::default().fg(Color::DarkGray),
        )
    };
    let help_line = Line::from(Span::styled(help_text, help_style));
    let help_para = Paragraph::new(help_line);
    let help_area = Rect::new(inner_area.x, inner_area.y + 2, inner_area.width, 1);
    frame.render_widget(help_para, help_area);

    render_separator(frame, inner_area, 3);

    render_hints(
        frame,
        inner_area,
        &[
            InputHint::new("Enter", "save"),
            InputHint::new("Esc", "cancel"),
        ],
        Color::Yellow,
    );
}
