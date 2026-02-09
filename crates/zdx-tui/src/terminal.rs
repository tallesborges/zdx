//! Terminal lifecycle management.
//!
//! This module handles terminal setup, restore, and panic hooks.
//! Terminal state is guaranteed to be restored on:
//! - Normal exit (via Drop)
//! - Ctrl+C signal
//! - Panic

use std::io::{self, Stdout};
use std::panic;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// Sets up the terminal for the TUI.
///
/// - Enables raw mode
/// - Enters alternate screen
/// - Creates the terminal instance
///
/// Call `install_panic_hook()` before this to ensure terminal restore on panic.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend).context("Failed to create terminal")?;
    Ok(terminal)
}

/// Enables additional terminal features for the TUI event loop.
///
/// - Enables bracketed paste mode
/// - Enables mouse capture
///
/// These are enabled separately from `setup_terminal()` because they need to be
/// disabled before `restore_terminal()` in normal exit paths, but `restore_terminal()`
/// will also disable them to handle panic/ctrl-c cases.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn enable_input_features() -> Result<()> {
    execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture)
        .context("Failed to enable input features")?;
    Ok(())
}

/// Disables additional terminal features enabled by `enable_input_features()`.
///
/// Call this before `restore_terminal()` in normal exit paths.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn disable_input_features() -> Result<()> {
    execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste)
        .context("Failed to disable input features")?;
    Ok(())
}

/// Restores terminal state.
///
/// - Disables mouse capture (safe to call even if not enabled)
/// - Disables bracketed paste (safe to call even if not enabled)
/// - Leaves alternate screen
/// - Disables raw mode
///
/// This function is idempotent and safe to call multiple times.
///
/// # Errors
/// Returns an error if the operation fails.
pub fn restore_terminal() -> Result<()> {
    // Disable mouse and bracketed paste first (safe even if not enabled)
    // These must be disabled before leaving raw mode
    let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);

    // Leave alternate screen (while still in raw mode)
    execute!(io::stdout(), LeaveAlternateScreen).context("Failed to leave alternate screen")?;
    disable_raw_mode().context("Failed to disable raw mode")?;
    Ok(())
}

/// Installs a panic hook that restores the terminal before printing the panic.
///
/// Call this BEFORE `setup_terminal()` to ensure terminal restore on panic.
pub fn install_panic_hook() {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        // Restore terminal first (includes mouse/paste cleanup)
        let _ = restore_terminal();
        // Then call the original panic hook
        original_hook(panic_info);
    }));
}

#[cfg(test)]
mod tests {
    // Note: Terminal tests are difficult to run in CI since they require a real TTY.
    // Key guarantees to test manually:
    // - Terminal is restored on normal exit (via Drop)
    // - Terminal is restored on panic
    // - Terminal is restored on Ctrl+C
    // - Mouse capture is disabled on all exit paths
    // - Bracketed paste is disabled on all exit paths
}
