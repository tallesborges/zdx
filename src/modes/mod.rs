//! Runtime execution modes.
//!
//! - `exec`: Non-interactive streaming mode (stdout/stderr)
//! - `tui`: Full-screen interactive terminal UI

pub mod exec;
pub mod tui;

pub use tui::{run_interactive_chat, run_interactive_chat_with_history};
