//! Gemini CLI (Cloud Code Assist OAuth) provider.

pub mod auth;
mod client;

pub use auth::GeminiCliConfig;
pub use client::GeminiCliClient;
