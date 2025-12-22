//! UI components for ZDX.
//!
//! This module provides terminal-based UI components:
//! - `tui2`: Full-screen alternate-screen TUI
//! - `chat`: Interactive chat interface using TUI2
//! - `stream`: Streaming renderer for exec mode

pub mod chat;
pub mod stream;
pub mod tui2;

pub use tui2::Tui2App;
