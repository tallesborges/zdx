//! Configuration-related pure value types.

use serde::{Deserialize, Serialize};

/// Thinking level for extended thinking feature.
///
/// Controls how much reasoning effort providers use before responding.
/// Higher levels use more tokens but provide deeper reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    /// No reasoning (default)
    #[default]
    Off,
    /// Low reasoning effort
    #[serde(alias = "minimal")]
    Low,
    /// Medium reasoning effort
    Medium,
    /// High reasoning effort
    High,
    /// Extended reasoning effort
    XHigh,
    /// Maximum available reasoning
    Max,
}

impl ThinkingLevel {
    /// Returns the token-budget percentage for providers that require one.
    /// Returns None for Off (thinking disabled).
    pub fn effort_percent(&self) -> Option<u32> {
        match self {
            ThinkingLevel::Off => None,
            ThinkingLevel::Low => Some(20),
            ThinkingLevel::Medium => Some(50),
            ThinkingLevel::High => Some(80),
            ThinkingLevel::XHigh | ThinkingLevel::Max => Some(95),
        }
    }

    /// Returns whether thinking is enabled for this level.
    pub fn is_enabled(&self) -> bool {
        !matches!(self, ThinkingLevel::Off)
    }

    /// Returns a human-readable description of this thinking level.
    pub fn description(&self) -> &'static str {
        match self {
            ThinkingLevel::Off => "No reasoning",
            ThinkingLevel::Low => "Fast and efficient",
            ThinkingLevel::Medium => "Balanced",
            ThinkingLevel::High => "Deep",
            ThinkingLevel::XHigh => "Extended",
            ThinkingLevel::Max => "Maximum capability",
        }
    }

    /// Returns the short display name for this level.
    pub fn display_name(&self) -> &'static str {
        match self {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::XHigh => "xhigh",
            ThinkingLevel::Max => "max",
        }
    }

    /// Parses a level from its [`Self::display_name`] (case-insensitive).
    /// Accepts `minimal` as an alias for `low` (matching the serde alias).
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "off" => Some(ThinkingLevel::Off),
            "low" | "minimal" => Some(ThinkingLevel::Low),
            "medium" => Some(ThinkingLevel::Medium),
            "high" => Some(ThinkingLevel::High),
            "xhigh" => Some(ThinkingLevel::XHigh),
            "max" => Some(ThinkingLevel::Max),
            _ => None,
        }
    }

    /// Returns all thinking levels for iteration (e.g., in picker).
    pub fn all() -> &'static [ThinkingLevel] {
        &[
            ThinkingLevel::Off,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::XHigh,
            ThinkingLevel::Max,
        ]
    }

    /// Computes the reasoning budget in tokens based on effort percent and `max_tokens`.
    ///
    /// `max_tokens` is expected to already be clamped to the model output limit
    /// by the caller. Uses min 1024 tokens to ensure meaningful reasoning.
    /// Returns None if thinking is Off.
    pub fn compute_reasoning_budget(&self, max_tokens: u32) -> Option<u32> {
        const MIN_BUDGET: u32 = 1024;

        let percent = self.effort_percent()?;

        let raw_budget =
            u32::try_from(u64::from(max_tokens) * u64::from(percent) / 100).unwrap_or(u32::MAX);

        Some(raw_budget.max(MIN_BUDGET))
    }
}

/// Text verbosity for `OpenAI` Responses-compatible providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TextVerbosity {
    Low,
    #[default]
    Medium,
    High,
}

impl TextVerbosity {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            TextVerbosity::Low => "low",
            TextVerbosity::Medium => "medium",
            TextVerbosity::High => "high",
        }
    }
}

/// Suffix marking the priority service tier (faster inference, 2× cost).
pub const FAST_MODIFIER: &str = "fast";

/// A parsed model spec: `provider:model` plus optional `@` modifiers.
///
/// Modifiers are order-independent trailing `@` segments, each either a
/// [`ThinkingLevel`] name or the literal `fast`. The canonical rendering is
/// `base[@thinking][@fast]`, e.g. `openai:gpt-5.2@high@fast`. Parsing stops at
/// the first unrecognized segment so a provider account prefix
/// (`claude-cli@work:sonnet-4`) is never consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec<'a> {
    /// Model id without modifiers, including any `provider:` prefix.
    pub base: &'a str,
    /// Explicit thinking level from an `@<level>` suffix.
    pub thinking: Option<ThinkingLevel>,
    /// Whether the `@fast` modifier is present.
    pub fast: bool,
}

impl<'a> ModelSpec<'a> {
    #[must_use]
    pub fn parse(spec: &'a str) -> Self {
        let mut base = spec.trim();
        let mut thinking = None;
        let mut fast = false;

        while let Some((head, suffix)) = base.rsplit_once('@') {
            if !fast && suffix.eq_ignore_ascii_case(FAST_MODIFIER) {
                fast = true;
            } else if thinking.is_none()
                && let Some(level) = ThinkingLevel::from_name(suffix)
            {
                thinking = Some(level);
            } else {
                break;
            }
            base = head;
        }

        Self {
            base,
            thinking,
            fast,
        }
    }

    /// Renders the spec without the thinking modifier, for callers that carry
    /// the thinking level in a separate field but must not drop `@fast`.
    #[must_use]
    pub fn without_thinking(&self) -> String {
        Self {
            base: self.base,
            thinking: None,
            fast: self.fast,
        }
        .to_string()
    }

    /// Returns the spec with `fast` set to the given value.
    #[must_use]
    pub fn with_fast(&self, fast: bool) -> Self {
        Self { fast, ..*self }
    }

    /// Returns the spec with an explicit thinking level, keeping `fast`.
    #[must_use]
    pub fn with_thinking(&self, thinking: ThinkingLevel) -> Self {
        Self {
            thinking: Some(thinking),
            ..*self
        }
    }
}

impl std::fmt::Display for ModelSpec<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.base)?;
        if let Some(level) = self.thinking {
            write!(f, "@{}", level.display_name())?;
        }
        if self.fast {
            write!(f, "@{FAST_MODIFIER}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelSpec, ThinkingLevel};

    #[test]
    fn legacy_minimal_deserializes_as_low() {
        let level: ThinkingLevel = serde_json::from_str("\"minimal\"").unwrap();
        assert_eq!(level, ThinkingLevel::Low);
        assert_eq!(serde_json::to_string(&level).unwrap(), "\"low\"");
    }

    #[test]
    fn model_spec_parses_modifiers_in_any_order() {
        let plain = ModelSpec::parse("gemini:x");
        assert_eq!(plain.base, "gemini:x");
        assert_eq!(plain.thinking, None);
        assert!(!plain.fast);

        let thinking = ModelSpec::parse("gemini:x@high");
        assert_eq!(thinking.base, "gemini:x");
        assert_eq!(thinking.thinking, Some(ThinkingLevel::High));
        assert!(!thinking.fast);

        for spec in ["openai:gpt-5@high@fast", "openai:gpt-5@fast@high"] {
            let parsed = ModelSpec::parse(spec);
            assert_eq!(parsed.base, "openai:gpt-5");
            assert_eq!(parsed.thinking, Some(ThinkingLevel::High));
            assert!(parsed.fast);
            assert_eq!(parsed.to_string(), "openai:gpt-5@high@fast");
        }
    }

    #[test]
    fn model_spec_leaves_unknown_suffix_and_account_alone() {
        let unknown = ModelSpec::parse("gemini:x@bogus");
        assert_eq!(unknown.base, "gemini:x@bogus");
        assert_eq!(unknown.thinking, None);
        assert!(!unknown.fast);

        let account = ModelSpec::parse("claude-cli@work:sonnet-4");
        assert_eq!(account.base, "claude-cli@work:sonnet-4");
        assert!(!account.fast);

        let account_fast = ModelSpec::parse("claude-cli@work:sonnet-4@fast");
        assert_eq!(account_fast.base, "claude-cli@work:sonnet-4");
        assert!(account_fast.fast);
    }

    #[test]
    fn model_spec_without_thinking_keeps_fast() {
        let spec = ModelSpec::parse("openai:gpt-5@max@fast");
        assert_eq!(spec.without_thinking(), "openai:gpt-5@fast");
        assert_eq!(spec.with_fast(false).to_string(), "openai:gpt-5@max");
    }
}
