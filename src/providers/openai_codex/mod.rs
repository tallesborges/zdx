//! OpenAI Codex (ChatGPT OAuth) provider.

pub mod auth;
mod client;
pub mod prompts;
mod sse;
mod types;

pub use auth::OpenAICodexConfig;
pub use client::OpenAICodexClient;
