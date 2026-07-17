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

#[cfg(test)]
mod tests {
    use super::ThinkingLevel;

    #[test]
    fn legacy_minimal_deserializes_as_low() {
        let level: ThinkingLevel = serde_json::from_str("\"minimal\"").unwrap();
        assert_eq!(level, ThinkingLevel::Low);
        assert_eq!(serde_json::to_string(&level).unwrap(), "\"low\"");
    }
}
