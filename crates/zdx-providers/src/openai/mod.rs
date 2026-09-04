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

/// Bare model id, with any `provider:` prefix stripped.
fn bare_model_id(model: &str) -> &str {
    model.rsplit(':').next().unwrap_or(model)
}

/// Models accepting the `max` reasoning effort on the Responses API.
fn supports_max_effort(model: &str) -> bool {
    let id = bare_model_id(model);
    id.starts_with("gpt-5.6") || id.starts_with("gpt-6")
}

/// Models that reject `none`/`minimal` effort because reasoning cannot be
/// disabled. `gpt-6-astra` only accepts `low`..=`max`.
fn requires_reasoning(model: &str) -> bool {
    bare_model_id(model).starts_with("gpt-6")
}

/// Reasoning effort for the first-party `OpenAI` Responses API (API-key and
/// Codex paths). Unlike the shared mapping, `Off` sends an explicit `"none"`:
/// omitting the field makes GPT-5.5/5.6 fall back to their `medium` default
/// instead of disabling reasoning. Models that cannot disable reasoning clamp
/// `Off` to their lowest accepted effort instead.
#[must_use]
pub fn responses_reasoning_effort(level: ThinkingLevel, model: &str) -> Option<&'static str> {
    match level {
        ThinkingLevel::Off if requires_reasoning(model) => Some("low"),
        ThinkingLevel::Off => Some("none"),
        ThinkingLevel::Max if supports_max_effort(model) => Some("max"),
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

    #[test]
    fn gpt_6_accepts_max_and_never_disables_reasoning() {
        for model in ["gpt-6-astra", "openai:gpt-6-astra"] {
            assert_eq!(
                responses_reasoning_effort(ThinkingLevel::Max, model),
                Some("max")
            );
            // `none` is rejected by gpt-6-astra, so `Off` clamps to `low`.
            assert_eq!(
                responses_reasoning_effort(ThinkingLevel::Off, model),
                Some("low")
            );
            assert_eq!(
                responses_reasoning_effort(ThinkingLevel::XHigh, model),
                Some("xhigh")
            );
        }
    }
}
