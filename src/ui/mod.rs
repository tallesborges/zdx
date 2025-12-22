//! UI components for ZDX.
//!
//! This module provides terminal-based UI components:
//! - `app`: Ratatui-based TUI with tui-textarea (inline viewport, fixed input at bottom)
//! - `tui2`: Full-screen alternate-screen TUI (WIP)

pub mod app;
pub mod chat;
pub mod stream;
pub mod tui2;

pub use app::{InputResult, TuiApp};
pub use tui2::Tui2App;
