//! Anthropic Claude API client.
//!
//! # Prompt Caching Strategy
//!
//! Anthropic allows up to 4 cache breakpoints per request. Each breakpoint caches
//! everything from the START of the request up to that marker (prefix caching).
//! Minimum cache size is 1,024 tokens.
//!
//! We use 2 breakpoints:
//! - **BP1 (last system block)**: Caches system prompt + AGENTS.md context.
//!   Reused across sessions with the same config.
//! - **BP2 (last user message)**: Caches conversation history.
//!   Reused within the same session for subsequent turns.
//!
//! This ensures the large system prompt is cached even for short conversations,
//! and provides cross-session cache hits when starting new conversations.

mod auth;
mod client;
mod errors;
mod sse;
mod types;

#[allow(unused_imports)]
pub use auth::{AnthropicConfig, AuthMethod};
pub use client::AnthropicClient;
pub use errors::{ProviderError, ProviderErrorKind};
#[allow(unused_imports)]
pub use sse::{SseParser, StreamEvent, Usage, parse_sse_event};
pub use types::{ChatContentBlock, ChatMessage, MessageContent};
