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

/// Reasoning effort for the first-party `OpenAI` Responses API (API-key and
/// Codex paths). Unlike the shared mapping, `Off` sends an explicit `"none"`:
/// omitting the field makes GPT-5.5/5.6 fall back to their `medium` default
/// instead of disabling reasoning.
pub(crate) fn responses_reasoning_effort(level: ThinkingLevel) -> Option<&'static str> {
    match level {
        ThinkingLevel::Off => Some("none"),
        other => reasoning_effort_from_thinking_level(other),
    }
}
