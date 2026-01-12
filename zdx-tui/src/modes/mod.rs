//! Runtime execution modes for the TUI crate.

pub mod tui;

pub use tui::{TuiRuntime, run_interactive_chat, run_interactive_chat_with_history};
