//! Gemini provider helpers and clients.

pub mod antigravity;
pub mod api;
pub mod shared;
pub mod sse;

pub use antigravity::{AntigravityClient, AntigravityConfig};
pub use api::{
    GeminiClient, GeminiConfig, GeminiImageGenerationOptions, GenerateImageResponse,
    GeneratedImage, SourceImage,
};
pub use shared::GeminiThinkingConfig;
