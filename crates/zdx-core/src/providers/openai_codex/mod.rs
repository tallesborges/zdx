//! OpenAI Codex (ChatGPT OAuth) provider.

pub mod auth;
mod client;

pub use auth::OpenAICodexConfig;
pub use client::OpenAICodexClient;
