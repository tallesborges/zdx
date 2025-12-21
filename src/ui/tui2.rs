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
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::core::interrupt;

/// Full-screen TUI application.
///
/// Uses the alternate screen buffer for a persistent interface.
/// Terminal state is guaranteed to be restored on drop, panic, or Ctrl+C.
pub struct Tui2App {
    /// Terminal instance.
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// Flag indicating the app should quit.
    should_quit: bool,
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

        Ok(Self {
            terminal,
            should_quit: false,
        })
    }

    /// Runs the main event loop.
    ///
    /// This blocks until the user quits (q or Ctrl+C).
    pub fn run(&mut self) -> Result<()> {
        while !self.should_quit {
            // Check for Ctrl+C signal (uses global interrupt flag)
            if interrupt::is_interrupted() {
                self.should_quit = true;
                break;
            }

            // Render
            self.terminal.draw(Self::render)?;

            // Handle events with timeout
            if event::poll(std::time::Duration::from_millis(100))? {
                self.handle_event(event::read()?)?;
            }
        }

        Ok(())
    }

    /// Renders the UI.
    fn render(frame: &mut Frame) {
        let area = frame.area();

        // Create layout: header, main content, footer
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Header
                Constraint::Min(1),    // Content
                Constraint::Length(3), // Footer/status
            ])
            .split(area);

        // Header
        let header = Paragraph::new(Line::from(vec![
            Span::styled("ZDX", Style::default().fg(Color::Cyan)),
            Span::raw(" — Full-Screen TUI"),
        ]))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(header, chunks[0]);

        // Main content area (blank for Slice 0)
        let content = Paragraph::new("").block(Block::default().borders(Borders::NONE));
        frame.render_widget(content, chunks[1]);

        // Footer with instructions
        let footer = Paragraph::new(Line::from(vec![
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw(" or "),
            Span::styled("Ctrl+C", Style::default().fg(Color::Yellow)),
            Span::raw(" to quit"),
        ]))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(footer, chunks[2]);
    }

    /// Handles a terminal event.
    fn handle_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Key(key) => self.handle_key(key),
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

        match key.code {
            // q: quit
            KeyCode::Char('q') if !ctrl => {
                self.should_quit = true;
            }
            // Ctrl+C: quit
            KeyCode::Char('c') if ctrl => {
                self.should_quit = true;
            }
            _ => {}
        }

        Ok(())
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
