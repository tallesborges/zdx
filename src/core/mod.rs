//! Core module: UI-agnostic domain and runtime.
//!
//! This module contains:
//! - `events`: Agent event types for streaming
//! - `context`: Project context loading (AGENTS.md files)
//! - `interrupt`: Signal handling for graceful interruption
//! - `agent`: Agent loop and event channels
//! - `session`: Session persistence

pub mod agent;
pub mod context;
pub mod events;
pub mod interrupt;
pub mod session;
