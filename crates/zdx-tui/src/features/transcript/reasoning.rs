//! Shared helpers for rendering reasoning/thinking blocks.
//!
//! Centralizes the decision of what visible text to show for a reasoning
//! block so the live-streaming path, the thread-rebuild path, and the
//! message-reload path stay in sync on edge cases like Anthropic
//! `redacted_thinking`.

use zdx_engine::providers::ReplayToken;

/// Placeholder shown for reasoning blocks with no user-visible plain text.
///
/// Emitted by Anthropic `redacted_thinking` content blocks, where the raw
/// chain-of-thought is encrypted and only the opaque `data` blob survives.
/// The TUI must still render an ordered, visible marker in the transcript
/// instead of silently dropping the block.
pub(crate) const REDACTED_REASONING_PLACEHOLDER: &str = "[redacted reasoning]";

/// Decides what to display for a reasoning block given its text and replay
/// token.
///
/// Return contract:
/// - `Some(visible)` when `text` is present and non-empty — visible text
///   always wins, even if `replay` is `ReplayToken::AnthropicRedacted`.
/// - `Some(REDACTED_REASONING_PLACEHOLDER)` when `text` is absent/empty but
///   `replay` is `ReplayToken::AnthropicRedacted`, so the redacted block
///   renders as a visible placeholder rather than being silently dropped.
/// - `None` otherwise — nothing visible to show; the caller skips the cell.
pub(crate) fn reasoning_display_text<'a>(
    text: Option<&'a str>,
    replay: Option<&ReplayToken>,
) -> Option<&'a str> {
    if let Some(visible) = text.filter(|s| !s.is_empty()) {
        return Some(visible);
    }
    if matches!(replay, Some(ReplayToken::AnthropicRedacted { .. })) {
        return Some(REDACTED_REASONING_PLACEHOLDER);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_visible_text_even_with_redacted_replay() {
        let replay = ReplayToken::AnthropicRedacted {
            data: "blob".to_string(),
        };
        assert_eq!(
            reasoning_display_text(Some("hello"), Some(&replay)),
            Some("hello")
        );
    }

    #[test]
    fn placeholder_when_text_missing_and_replay_redacted() {
        let replay = ReplayToken::AnthropicRedacted {
            data: "blob".to_string(),
        };
        assert_eq!(
            reasoning_display_text(None, Some(&replay)),
            Some(REDACTED_REASONING_PLACEHOLDER)
        );
    }

    #[test]
    fn placeholder_when_text_empty_and_replay_redacted() {
        let replay = ReplayToken::AnthropicRedacted {
            data: "blob".to_string(),
        };
        assert_eq!(
            reasoning_display_text(Some(""), Some(&replay)),
            Some(REDACTED_REASONING_PLACEHOLDER)
        );
    }

    #[test]
    fn none_when_no_text_and_no_redacted_replay() {
        assert_eq!(reasoning_display_text(None, None), None);
        let signed = ReplayToken::Anthropic {
            signature: "sig".to_string(),
        };
        assert_eq!(reasoning_display_text(None, Some(&signed)), None);
    }
}
