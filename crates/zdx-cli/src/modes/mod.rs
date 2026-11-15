//! Runtime execution modes.
//!
//! - `exec`: Non-interactive streaming mode (stdout/stderr)
//!
//! The interactive chat is the Go TUI (`apps/tui`); `commands::chat` starts it.

pub mod exec;
