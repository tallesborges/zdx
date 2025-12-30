//! User input state.
//!
//! Manages the text area, command history, and history navigation.

/// User input state.
///
/// Encapsulates the text area, command history, and navigation state.
pub struct InputState {
    /// Text area for user input.
    pub textarea: tui_textarea::TextArea<'static>,

    /// Command history for ↑/↓ navigation.
    pub history: Vec<String>,

    /// Current position in history (None = not navigating).
    pub history_index: Option<usize>,

    /// Draft text saved when navigating history.
    pub draft: Option<String>,

    /// Whether we're in handoff mode (next submit creates handoff).
    pub handoff_pending: bool,
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

impl InputState {
    /// Creates a new InputState with default textarea styling.
    pub fn new() -> Self {
        use ratatui::style::{Color, Style};
        use ratatui::widgets::{Block, Borders};

        let mut textarea = tui_textarea::TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Input (Enter=send, Shift+Enter=newline, Ctrl+J=newline) "),
        );

        Self {
            textarea,
            history: Vec::new(),
            history_index: None,
            draft: None,
            handoff_pending: false,
        }
    }

    /// Gets the current input text.
    pub fn get_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Clears the input textarea.
    pub fn clear(&mut self) {
        self.textarea.select_all();
        self.textarea.cut();
        self.reset_navigation();
    }

    /// Sets the input textarea to the given text.
    pub fn set_text(&mut self, text: &str) {
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.insert_str(text);
    }

    /// Resets history navigation state.
    pub fn reset_navigation(&mut self) {
        self.history_index = None;
        self.draft = None;
    }

    /// Returns true if up arrow should navigate history (not move cursor).
    pub fn should_navigate_up(&self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        if self.history_index.is_some() {
            return true;
        }
        if self.get_text().is_empty() {
            return true;
        }
        let (row, _col) = self.textarea.cursor();
        row == 0
    }

    /// Returns true if down arrow should navigate history (not move cursor).
    pub fn should_navigate_down(&self) -> bool {
        if self.history_index.is_none() {
            return false;
        }
        let (row, _col) = self.textarea.cursor();
        let line_count = self.textarea.lines().len();
        row >= line_count.saturating_sub(1)
    }

    /// Navigates up in command history.
    pub fn navigate_up(&mut self) {
        if self.history.is_empty() {
            return;
        }

        if self.history_index.is_none() {
            let current = self.get_text();
            self.draft = Some(current);
            self.history_index = Some(self.history.len() - 1);
        } else if let Some(idx) = self.history_index
            && idx > 0
        {
            self.history_index = Some(idx - 1);
        }

        if let Some(idx) = self.history_index
            && let Some(entry) = self.history.get(idx).cloned()
        {
            self.set_text(&entry);
        }
    }

    /// Navigates down in command history.
    pub fn navigate_down(&mut self) {
        let Some(idx) = self.history_index else {
            return;
        };

        if idx + 1 < self.history.len() {
            self.history_index = Some(idx + 1);
            if let Some(entry) = self.history.get(idx + 1).cloned() {
                self.set_text(&entry);
            }
        } else {
            let draft = self.draft.take().unwrap_or_default();
            self.history_index = None;
            self.set_text(&draft);
        }
    }
}
