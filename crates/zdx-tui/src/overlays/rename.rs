//! Rename overlay for thread renaming.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::OverlayUpdate;
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
        InputHint, InputLine, OverlayConfig, render_input_line, render_overlay, render_separator,
    };

    let overlay_width = 50;
    let overlay_height = 7;

    let hints = [
        InputHint::new("Enter", "save"),
        InputHint::new("Esc", "cancel"),
    ];
    let layout = render_overlay(
        frame,
        area,
        input_top_y,
        &OverlayConfig {
            title: "Rename Thread",
            border_color: Color::Yellow,
            width: overlay_width,
            height: overlay_height,
            hints: &hints,
        },
    );

    // Input line with unicode-safe truncation
    let placeholder = state
        .current_title
        .as_deref()
        .unwrap_or("Enter new title...");
    let input_area = Rect::new(layout.body.x, layout.body.y, layout.body.width, 1);
    render_input_line(
        frame,
        input_area,
        &InputLine {
            value: &state.input,
            placeholder: Some(placeholder),
            prompt: "> ",
            prompt_color: Color::DarkGray,
            text_color: Color::Yellow,
            placeholder_color: Color::DarkGray,
            cursor_color: Color::Yellow,
        },
    );

    render_separator(frame, layout.body, 1);

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
    let help_area = Rect::new(layout.body.x, layout.body.y + 2, layout.body.width, 1);
    frame.render_widget(help_para, help_area);

    render_separator(frame, layout.body, 3);
}
