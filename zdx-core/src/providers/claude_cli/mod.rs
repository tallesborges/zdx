//! Claude CLI (Anthropic OAuth) provider.

pub mod auth;
mod client;

pub use auth::ClaudeCliConfig;
pub use client::ClaudeCliClient;
