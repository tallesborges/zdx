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
        ThinkingLevel::Low => Some("low"),
        ThinkingLevel::Medium => Some("medium"),
        ThinkingLevel::High => Some("high"),
        ThinkingLevel::XHigh | ThinkingLevel::Max => Some("xhigh"),
    }
}

/// Reasoning effort for the first-party `OpenAI` Responses API (API-key and
/// Codex paths). Unlike the shared mapping, `Off` sends an explicit `"none"`:
/// omitting the field makes GPT-5.5/5.6 fall back to their `medium` default
/// instead of disabling reasoning.
pub(crate) fn responses_reasoning_effort(
    level: ThinkingLevel,
    model: &str,
) -> Option<&'static str> {
    match level {
        ThinkingLevel::Off => Some("none"),
        ThinkingLevel::Max
            if model
                .rsplit(':')
                .next()
                .is_some_and(|id| id.starts_with("gpt-5.6")) =>
        {
            Some("max")
        }
        other => reasoning_effort_from_thinking_level(other),
    }
}

#[cfg(test)]
mod tests {
    use zdx_types::ThinkingLevel;

    use super::{reasoning_effort_from_thinking_level, responses_reasoning_effort};

    #[test]
    fn generic_openai_compatible_max_clamps_to_xhigh() {
        assert_eq!(
            reasoning_effort_from_thinking_level(ThinkingLevel::Max),
            Some("xhigh")
        );
    }

    #[test]
    fn first_party_openai_max_is_model_aware() {
        assert_eq!(
            responses_reasoning_effort(ThinkingLevel::Max, "gpt-5.6-sol"),
            Some("max")
        );
        assert_eq!(
            responses_reasoning_effort(ThinkingLevel::Max, "openai:gpt-5.6"),
            Some("max")
        );
        assert_eq!(
            responses_reasoning_effort(ThinkingLevel::Max, "gpt-5.5"),
            Some("xhigh")
        );
        assert_eq!(
            responses_reasoning_effort(ThinkingLevel::Off, "gpt-5.6-sol"),
            Some("none")
        );
    }
}
