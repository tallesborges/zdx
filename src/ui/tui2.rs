//! Full-screen alternate-screen TUI (TUI2).
//!
//! This module provides a full-screen terminal UI using ratatui.
//! Unlike the inline-viewport TUI in `app.rs`, this uses the alternate
//! screen buffer for a persistent, scrollable interface.
//!
//! See docs/plans/plan_ratatui_full_screen_tui2.md for the implementation plan.

use std::io::{self, Stdout};
use std::panic;

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use tui_textarea::TextArea;

use crate::core::interrupt;
use crate::core::transcript::{HistoryCell, StyledLine, Style as TranscriptStyle};

/// Height of the input area (lines).
const INPUT_HEIGHT: u16 = 5;

/// Full-screen TUI application.
///
/// Uses the alternate screen buffer for a persistent interface.
/// Terminal state is guaranteed to be restored on drop, panic, or Ctrl+C.
pub struct Tui2App {
    /// Terminal instance.
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// Flag indicating the app should quit.
    should_quit: bool,
    /// Text area for input.
    textarea: TextArea<'static>,
    /// Transcript cells (in-memory, no persistence yet).
    transcript: Vec<HistoryCell>,
}

impl Tui2App {
    /// Creates a new TUI2 application.
    ///
    /// This enters the alternate screen and enables raw mode.
    /// Terminal state will be restored when the app is dropped.
    pub fn new() -> Result<Self> {
        // Set up panic hook BEFORE entering alternate screen
        install_panic_hook();

        // Reset interrupt flag in case it was set from a previous run
        interrupt::reset();

        // Enter alternate screen and raw mode
        let terminal = setup_terminal().context("Failed to setup terminal")?;

        // Set up textarea with styling
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Input (Enter=send, Shift+Enter=newline) "),
        );

        Ok(Self {
            terminal,
            should_quit: false,
            textarea,
            transcript: Vec::new(),
        })
    }

    /// Runs the main event loop.
    ///
    /// This blocks until the user quits (q or Ctrl+C).
    pub fn run(&mut self) -> Result<()> {
        // Enable bracketed paste for proper paste handling
        execute!(io::stdout(), event::EnableBracketedPaste)?;

        let result = self.event_loop();

        // Disable bracketed paste
        execute!(io::stdout(), event::DisableBracketedPaste)?;

        result
    }

    fn event_loop(&mut self) -> Result<()> {
        while !self.should_quit {
            // Check for Ctrl+C signal (uses global interrupt flag)
            if interrupt::is_interrupted() {
                self.should_quit = true;
                break;
            }

            // Render
            self.render()?;

            // Handle events with timeout
            if event::poll(std::time::Duration::from_millis(100))? {
                self.handle_event(event::read()?)?;
            }
        }

        Ok(())
    }

    /// Renders the UI.
    fn render(&mut self) -> Result<()> {
        // Get terminal size for transcript rendering
        let size = self.terminal.size()?;
        let transcript_width = size.width.saturating_sub(2) as usize;

        // Pre-render transcript lines (avoids borrow issues in closure)
        let transcript_lines = self.render_transcript(transcript_width);

        // Clone textarea for rendering (tui-textarea doesn't impl Copy)
        let textarea = &self.textarea;

        self.terminal.draw(|frame| {
            let area = frame.area();

            // Create layout: header, transcript, input
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),            // Header
                    Constraint::Min(1),               // Transcript
                    Constraint::Length(INPUT_HEIGHT), // Input
                ])
                .split(area);

            // Header
            let header = Paragraph::new(Line::from(vec![
                Span::styled(
                    "ZDX",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" — "),
                Span::styled("q", Style::default().fg(Color::Yellow)),
                Span::raw(" to quit"),
            ]))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
            frame.render_widget(header, chunks[0]);

            // Transcript area
            let transcript = Paragraph::new(transcript_lines.clone())
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::NONE));
            frame.render_widget(transcript, chunks[1]);

            // Input area
            frame.render_widget(textarea, chunks[2]);
        })?;

        Ok(())
    }

    /// Renders the transcript into ratatui Lines.
    fn render_transcript(&self, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        for cell in &self.transcript {
            let styled_lines = cell.display_lines(width);
            for styled_line in styled_lines {
                lines.push(self.convert_styled_line(styled_line));
            }
            // Add blank line between cells
            lines.push(Line::default());
        }

        // Remove trailing blank line if present
        if lines.last().map(|l| l.spans.is_empty()).unwrap_or(false) {
            lines.pop();
        }

        lines
    }

    /// Converts a transcript StyledLine to a ratatui Line.
    fn convert_styled_line(&self, styled_line: StyledLine) -> Line<'static> {
        let spans: Vec<Span<'static>> = styled_line
            .spans
            .into_iter()
            .map(|s| {
                let style = self.convert_style(s.style);
                Span::styled(s.text, style)
            })
            .collect();
        Line::from(spans)
    }

    /// Converts a transcript Style to a ratatui Style.
    fn convert_style(&self, style: TranscriptStyle) -> Style {
        match style {
            TranscriptStyle::Plain => Style::default(),
            TranscriptStyle::UserPrefix => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            TranscriptStyle::User => Style::default().fg(Color::White),
            TranscriptStyle::AssistantPrefix => Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            TranscriptStyle::Assistant => Style::default().fg(Color::White),
            TranscriptStyle::StreamingCursor => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::SLOW_BLINK),
            TranscriptStyle::SystemPrefix => Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            TranscriptStyle::System => Style::default().fg(Color::DarkGray),
            TranscriptStyle::ToolBracket => Style::default().fg(Color::DarkGray),
            TranscriptStyle::ToolStatus => Style::default().fg(Color::Yellow),
            TranscriptStyle::ToolError => Style::default().fg(Color::Red),
        }
    }

    /// Handles a terminal event.
    fn handle_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Paste(text) => {
                self.textarea.insert_str(&text);
                Ok(())
            }
            Event::Resize(_, _) => {
                // Ratatui handles resize automatically
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Handles a key event.
    fn handle_key(&mut self, key: event::KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            // q without modifiers: quit (only when input is empty)
            KeyCode::Char('q') if !ctrl && !shift && !alt => {
                if self.get_input_text().is_empty() {
                    self.should_quit = true;
                } else {
                    self.textarea.input(key);
                }
            }
            // Ctrl+C: quit
            KeyCode::Char('c') if ctrl => {
                self.should_quit = true;
            }
            // Enter: submit (unless Shift+Enter or Alt+Enter for newline)
            KeyCode::Enter if !shift && !alt => {
                self.submit_input();
            }
            // Shift+Enter or Alt+Enter: insert newline
            KeyCode::Enter if shift || alt => {
                self.textarea.insert_newline();
            }
            // Escape: clear input
            KeyCode::Esc => {
                self.clear_input();
            }
            // Pass everything else to textarea
            _ => {
                self.textarea.input(key);
            }
        }

        Ok(())
    }

    /// Gets the current input text.
    fn get_input_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Clears the input textarea.
    fn clear_input(&mut self) {
        self.textarea.select_all();
        self.textarea.cut();
    }

    /// Submits the current input.
    fn submit_input(&mut self) {
        let text = self.get_input_text();
        if text.trim().is_empty() {
            return;
        }

        // Add user cell to transcript
        self.transcript.push(HistoryCell::user(&text));

        // Clear input
        self.clear_input();
    }
}

impl Drop for Tui2App {
    fn drop(&mut self) {
        // Restore terminal state
        let _ = restore_terminal();
    }
}

/// Sets up the terminal for the TUI.
///
/// - Enables raw mode
/// - Enters alternate screen
/// - Creates the terminal instance
fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend).context("Failed to create terminal")?;
    Ok(terminal)
}

/// Restores terminal state.
///
/// - Leaves alternate screen
/// - Disables raw mode
fn restore_terminal() -> Result<()> {
    // Leave alternate screen first (while still in raw mode)
    execute!(io::stdout(), LeaveAlternateScreen).context("Failed to leave alternate screen")?;
    disable_raw_mode().context("Failed to disable raw mode")?;
    Ok(())
}

/// Installs a panic hook that restores the terminal before printing the panic.
fn install_panic_hook() {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        // Restore terminal first
        let _ = restore_terminal();
        // Then call the original panic hook
        original_hook(panic_info);
    }));
}

#[cfg(test)]
mod tests {
    // Note: Terminal tests are difficult to run in CI since they require a real TTY.
    // Integration tests for TUI2 should spawn the CLI and verify stdout/stderr behavior.
    //
    // Key guarantees to test manually or via integration tests:
    // - Terminal is restored on normal exit
    // - Terminal is restored on panic
    // - Terminal is restored on Ctrl+C
    // - Resize events don't break the UI
}
