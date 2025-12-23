//! UI components for ZDX.
//!
//! This module provides terminal-based UI components:
//! - `tui`: Full-screen alternate-screen TUI for interactive chat
//! - `stream`: Streaming renderer for exec mode
//! - `transcript`: Virtual transcript model for chat cells
//! - `terminal`: Terminal lifecycle management (setup/restore/panic hook)

pub mod stream;
pub mod terminal;
pub mod transcript;
pub mod tui;

pub use tui::{run_interactive_chat, run_interactive_chat_with_history};
