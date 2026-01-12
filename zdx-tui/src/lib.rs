//! Full-screen TUI implementation for ZDX.

pub mod modes;
pub mod terminal;

pub use modes::tui::{TuiRuntime, run_interactive_chat, run_interactive_chat_with_history};
