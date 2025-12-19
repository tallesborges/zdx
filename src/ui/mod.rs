//! UI components for ZDX.
//!
//! This module provides terminal-based UI components:
//! - `tui`: Ratatui-based TUI with tui-textarea (inline viewport, fixed input at bottom)

pub mod tui;

pub use tui::{InputResult, TuiApp};
