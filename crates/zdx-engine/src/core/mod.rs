//! Core module: UI-agnostic domain and runtime.
//!
//! This module contains:
//! - `events`: Agent event types for streaming
//! - `fts_query`: shared FTS5 MATCH expression builder for the search indexes
//! - `context`: Project context loading (AGENTS.md files)
//! - `interrupt`: Signal handling for graceful interruption
//! - `agent`: Agent loop and event channels
//! - `handoff_generation`: LLM-based handoff context generation
//! - `prompt_builder_generation`: LLM-based prompt-builder generation
//! - `recency`: shared recency decay applied to search relevance scores
//! - `media_fallback`: image-to-`ask-media` fallback for models without image input
//! - `native_memory`: native SQLite-backed memory index/search
//! - `subagent`: Child `zdx exec` subagent runner
//! - `thread_export`: Thread transcript exports
//! - `thread_index`: derived `threads.sqlite` cache (metadata, FTS, tool rows, export dirty state)
//! - `thread_persistence`: Thread persistence
//! - `thread_timing`: Per-thread client-observed timing reduction and formatting
//! - `title_generation`: LLM-based title generation
//! - `tldr_generation`: LLM-based thread TLDR/recap generation
//! - `usage_stats`: Usage/cost aggregation over saved threads
//! - `worktree`: Git worktree management helpers

pub mod agent;
pub mod context;
pub mod events;
pub mod fts_query;
pub mod handoff_generation;
pub mod interrupt;
pub mod media_fallback;
pub mod native_memory;
pub mod prompt_builder_generation;
pub mod recency;
pub mod subagent;
pub mod thread_export;
pub mod thread_index;
pub mod thread_persistence;
pub mod thread_timing;
pub mod title_generation;
pub mod tldr_generation;
pub mod usage_stats;
pub mod worktree;
