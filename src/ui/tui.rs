//! Minimal ratatui TUI for chat.
//!
//! Uses inline viewport: messages print normally above, input fixed at bottom.
//! Uses tui-textarea for input handling.

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    style::{Color, Style},
    widgets::{Block, Borders},
};
use tui_textarea::TextArea;

/// Height of the input area (lines).
const INPUT_HEIGHT: u16 = 3;

/// Result from input handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputResult {
    /// Continue (no action needed).
    Continue,
    /// Submit the text.
    Submit(String),
    /// Clear the buffer.
    Clear,
    /// Quit the application.
    Quit,
}

/// Minimal TUI application with inline viewport.
pub struct TuiApp {
    /// Text area for input.
    textarea: TextArea<'static>,
    /// History of user messages.
    history: Vec<String>,
    /// Current history index (None = editing new, Some(i) = viewing history).
    history_index: Option<usize>,
    /// Stashed text when navigating history.
    stashed: String,
    /// Pending Ctrl+C for double-tap quit.
    pending_ctrl_c: bool,
    /// Prompt string (owned).
    prompt: String,
}

impl TuiApp {
    pub fn new(prompt: &str, history: Vec<String>) -> Self {
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(prompt.to_string()),
        );

        Self {
            textarea,
            history,
            history_index: None,
            stashed: String::new(),
            pending_ctrl_c: false,
            prompt: prompt.to_string(),
        }
    }

    /// Add to history.
    pub fn push_history(&mut self, msg: String) {
        if !msg.trim().is_empty() {
            self.history.push(msg);
        }
    }

    /// Reset input after submit.
    pub fn reset_input(&mut self) {
        self.textarea = TextArea::default();
        self.textarea.set_cursor_line_style(Style::default());
        self.textarea.set_block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(self.prompt.clone()),
        );
        self.history_index = None;
        self.stashed.clear();
        self.pending_ctrl_c = false;
    }

    /// Get the current text.
    fn get_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Set the textarea content.
    fn set_text(&mut self, text: &str) {
        // Clear and insert new text
        self.textarea.select_all();
        self.textarea.cut(); // Remove all
        self.textarea.insert_str(text);
    }

    /// Read input from user.
    pub fn read_input(&mut self) -> Result<InputResult> {
        // Create fresh terminal with inline viewport each time
        let mut terminal = setup_terminal()?;

        enable_raw_mode()?;
        execute!(io::stdout(), event::EnableBracketedPaste)?;

        let result = self.run_input_loop(&mut terminal);

        execute!(io::stdout(), event::DisableBracketedPaste)?;
        disable_raw_mode()?;

        // Clear the viewport area and drop terminal
        terminal.clear()?;
        drop(terminal);

        result
    }

    fn run_input_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> Result<InputResult> {
        loop {
            // Render
            terminal.draw(|f| {
                f.render_widget(&self.textarea, f.area());
            })?;

            if event::poll(std::time::Duration::from_millis(100))? {
                let ev = event::read()?;
                let result = self.handle_event(ev);
                match result {
                    InputResult::Continue => {}
                    _ => return Ok(result),
                }
            }
        }
    }

    fn handle_event(&mut self, ev: Event) -> InputResult {
        match ev {
            Event::Key(key) => self.handle_key(key),
            Event::Paste(text) => {
                self.textarea.insert_str(&text);
                self.pending_ctrl_c = false;
                InputResult::Continue
            }
            _ => InputResult::Continue,
        }
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> InputResult {
        use KeyCode::*;

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        // Handle special keys before passing to textarea
        match key.code {
            // Enter: submit (unless Shift+Enter for newline)
            Enter if !shift && !alt => {
                let text = self.get_text();
                self.pending_ctrl_c = false;
                return InputResult::Submit(text);
            }

            // Shift+Enter or Alt+Enter: insert newline
            Enter if shift || alt => {
                self.textarea.insert_newline();
                self.pending_ctrl_c = false;
                return InputResult::Continue;
            }

            // Ctrl+C: clear or quit (double-tap)
            Char('c') if ctrl => {
                if self.pending_ctrl_c {
                    return InputResult::Quit;
                }
                if self.get_text().is_empty() {
                    self.pending_ctrl_c = true;
                } else {
                    self.set_text("");
                    self.pending_ctrl_c = false;
                }
                return InputResult::Clear;
            }

            // Escape: clear buffer
            Esc => {
                if self.get_text().is_empty() {
                    self.pending_ctrl_c = true;
                } else {
                    self.set_text("");
                }
                return InputResult::Clear;
            }

            // Up arrow at first line: history navigation
            Up if !shift && !ctrl && !alt => {
                let (row, _) = self.textarea.cursor();
                if row == 0 && !self.history.is_empty() {
                    self.navigate_history_up();
                    return InputResult::Continue;
                }
            }

            // Down arrow at last line: history navigation
            Down if !shift && !ctrl && !alt => {
                let (row, _) = self.textarea.cursor();
                let last_row = self.textarea.lines().len().saturating_sub(1);
                if row == last_row && self.history_index.is_some() {
                    self.navigate_history_down();
                    return InputResult::Continue;
                }
            }

            _ => {}
        }

        // Pass to textarea for default handling
        self.textarea.input(key);
        self.pending_ctrl_c = false;
        InputResult::Continue
    }

    fn navigate_history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }

        match self.history_index {
            None => {
                // Stash current text and go to most recent history
                self.stashed = self.get_text();
                self.history_index = Some(self.history.len() - 1);
            }
            Some(0) => {
                // Already at oldest, do nothing
                return;
            }
            Some(i) => {
                self.history_index = Some(i - 1);
            }
        }

        if let Some(i) = self.history_index {
            self.set_text(&self.history[i].clone());
        }
    }

    fn navigate_history_down(&mut self) {
        match self.history_index {
            None => (),
            Some(i) if i >= self.history.len() - 1 => {
                // At most recent, restore stashed
                self.history_index = None;
                let stashed = self.stashed.clone();
                self.set_text(&stashed);
            }
            Some(i) => {
                self.history_index = Some(i + 1);
                self.set_text(&self.history[i + 1].clone());
            }
        }
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    let backend = CrosstermBackend::new(io::stdout());
    let options = TerminalOptions {
        viewport: Viewport::Inline(INPUT_HEIGHT),
    };
    let terminal = Terminal::with_options(backend, options)?;
    Ok(terminal)
}
