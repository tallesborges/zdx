//! Anthropic Claude API providers (API key + Claude CLI OAuth).
//!
//! # Prompt Caching Strategy
//!
//! Anthropic allows up to 4 cache breakpoints per request. Each breakpoint caches
//! everything from the START of the request up to that marker (prefix caching).
//! Minimum cache size is 1,024 tokens.
//!
//! We use 2 breakpoints:
//! - **BP1 (last system block)**: Caches system prompt + AGENTS.md context.
//!   Reused across threads with the same config.
//! - **BP2 (last user message)**: Caches thread history.
//!   Reused within the same thread for subsequent turns.
//!
//! This ensures the large system prompt is cached even for short conversations,
//! and provides cross-thread cache hits when starting new conversations.

pub mod api;
pub mod cli;
mod shared;
mod sse;
pub(crate) mod types;

pub use api::{AnthropicClient, AnthropicConfig, DEFAULT_BASE_URL};
pub use cli::{ClaudeCliClient, ClaudeCliConfig};
pub use types::EffortLevel;
