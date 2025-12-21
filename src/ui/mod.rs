//! UI components for ZDX.
//!
//! This module provides terminal-based UI components:
//! - `app`: Ratatui-based TUI with tui-textarea (inline viewport, fixed input at bottom)

pub mod app;
pub mod chat;

pub use app::{InputResult, TuiApp};
