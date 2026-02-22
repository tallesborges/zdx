//! Core module: UI-agnostic domain and runtime.
//!
//! This module contains:
//! - `events`: Agent event types for streaming
//! - `context`: Project context loading (AGENTS.md files)
//! - `interrupt`: Signal handling for graceful interruption
//! - `agent`: Agent loop and event channels
//! - `subagent`: Child `zdx exec` subagent runner
//! - `thread_persistence`: Thread persistence
//! - `title_generation`: LLM-based title generation
//! - `worktree`: Git worktree management helpers

pub mod agent;
pub mod context;
pub mod events;
pub mod interrupt;
pub mod subagent;
pub mod thread_persistence;
pub mod title_generation;
pub mod worktree;
