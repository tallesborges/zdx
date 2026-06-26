//! OpenAI-compatible provider helpers and clients.

use zdx_types::config::ThinkingLevel;

pub mod api;
pub mod chat_completions;
pub mod codex;
pub mod image_generation;
pub mod responses;
mod responses_sse;
mod responses_types;
pub mod responses_ws;

pub use api::{OpenAIClient, OpenAIConfig};
pub use codex::{OpenAICodexClient, OpenAICodexConfig};
pub use image_generation::{
    OpenAIGenerateImageResponse, OpenAIGeneratedImage, OpenAIImageGenerationOptions,
    OpenAIImageInput,
};
pub use responses_ws::OpenAIResponsesWsClient;

/// Maps a ZDX thinking level to the `reasoning_effort` vocabulary used by
/// `OpenAI`-compatible providers. Returns `None` when thinking is off.
pub(crate) fn reasoning_effort_from_thinking_level(level: ThinkingLevel) -> Option<&'static str> {
    match level {
        ThinkingLevel::Off => None,
        ThinkingLevel::Minimal | ThinkingLevel::Low => Some("low"),
        ThinkingLevel::Medium => Some("medium"),
        ThinkingLevel::High => Some("high"),
        ThinkingLevel::XHigh => Some("xhigh"),
    }
}
