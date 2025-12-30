//! UI components for ZDX.
//!
//! This module is organized by mode:
//! - `chat`: Full-screen TUI for interactive chat
//! - `exec`: Streaming renderer for exec mode
//! - `transcript`: Virtual transcript model shared by chat
//! - `markdown`: Markdown parsing and wrapping shared by chat

pub mod chat;
pub mod exec;
pub mod markdown;
pub mod transcript;

pub use chat::{run_interactive_chat, run_interactive_chat_with_history};
