//! UI components for ZDX.
//!
//! This module provides terminal-based UI components:
//! - `tui`: Full-screen alternate-screen TUI runtime for interactive chat
//! - `state`: TUI application state (separate from terminal ownership)
//! - `view`: Pure render functions (read-only, no mutations)
//! - `commands`: Slash command definitions
//! - `stream`: Streaming renderer for exec mode
//! - `transcript`: Virtual transcript model for chat cells
//! - `terminal`: Terminal lifecycle management (setup/restore/panic hook)

pub mod commands;
pub mod state;
pub mod stream;
pub mod terminal;
pub mod transcript;
pub mod tui;
pub mod view;

pub use tui::{run_interactive_chat, run_interactive_chat_with_history};
