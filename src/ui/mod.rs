//! UI components for ZDX.
//!
//! This module provides terminal-based UI components:
//! - `tui`: Ratatui-based TUI with tui-textarea (inline viewport, fixed input at bottom)

pub mod tui;

pub use tui::{InputResult, TuiApp};

/// Checks if stdin is a TTY.
pub fn is_tty() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdin())
}
