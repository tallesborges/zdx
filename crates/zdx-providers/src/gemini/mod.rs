//! Gemini provider helpers and clients.

pub mod antigravity;
pub mod api;
pub mod cli;
pub mod shared;
pub mod sse;

pub use antigravity::{AntigravityClient, AntigravityConfig};
pub use api::{
    GeminiClient, GeminiConfig, GeminiImageGenerationOptions, GenerateImageResponse,
    GeneratedImage, SourceImage,
};
pub use cli::{GeminiCliClient, GeminiCliConfig};
pub use shared::GeminiThinkingConfig;
