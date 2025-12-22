//! Core module: UI-agnostic domain and runtime.
//!
//! This module contains:
//! - `events`: Engine event types for streaming
//! - `context`: Project context loading (AGENTS.md files)
//! - `interrupt`: Signal handling for graceful interruption
//! - `engine`: Engine loop and event channels
//! - `session`: Session persistence

pub mod context;
pub mod engine;
pub mod events;
pub mod interrupt;
pub mod session;
