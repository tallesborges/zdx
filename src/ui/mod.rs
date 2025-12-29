//! UI components for ZDX.
//!
//! This module provides terminal-based UI components:
//! - `tui`: Full-screen alternate-screen TUI runtime for interactive chat
//! - `state`: TUI application state (separate from terminal ownership)
//! - `events`: Unified event enum for the TUI
//! - `effects`: Effect types returned by the reducer
//! - `update`: The reducer - all state mutations happen here
//! - `view`: Pure render functions (read-only, no mutations)
//! - `overlays`: Overlay components (command palette, model picker, login)
//! - `commands`: Slash command definitions
//! - `stream`: Streaming renderer for exec mode
//! - `transcript`: Virtual transcript model for chat cells
//! - `terminal`: Terminal lifecycle management (setup/restore/panic hook)
//! - `markdown`: Markdown parsing and styled text wrapping for assistant responses
//! - `selection`: Transcript text selection and copy functionality

pub mod commands;
pub mod effects;
pub mod events;
pub mod markdown;
pub mod overlays;
pub mod selection;
pub mod state;
pub mod stream;
pub mod terminal;
pub mod transcript;
pub mod tui;
pub mod update;
pub mod view;

pub use tui::{run_interactive_chat, run_interactive_chat_with_history};
