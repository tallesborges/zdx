//! Configuration-related pure value types.

use serde::{Deserialize, Serialize};

/// Thinking level for extended thinking feature.
///
/// Controls how much reasoning Claude shows before responding.
/// Higher levels use more tokens but provide deeper reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    /// No reasoning (default)
    #[default]
    Off,
    /// Very brief reasoning (~5% of max tokens)
    Minimal,
    /// Light reasoning (~20% of max tokens)
    Low,
    /// Moderate reasoning (~50% of max tokens)
    Medium,
    /// Deep reasoning (~80% of max tokens)
    High,
    /// Very deep reasoning (~95% of max tokens)
    XHigh,
}

impl ThinkingLevel {
    /// Returns the effort percentage of max tokens for this thinking level.
    /// Returns None for Off (thinking disabled).
    pub fn effort_percent(&self) -> Option<u32> {
        match self {
            ThinkingLevel::Off => None,
            ThinkingLevel::Minimal => Some(5),
            ThinkingLevel::Low => Some(20),
            ThinkingLevel::Medium => Some(50),
            ThinkingLevel::High => Some(80),
            ThinkingLevel::XHigh => Some(95),
        }
    }

    /// Returns the normalized effort label for this level.
    pub fn effort_label(&self) -> Option<&'static str> {
        match self {
            ThinkingLevel::Off => None,
            ThinkingLevel::Minimal => Some("minimal"),
            ThinkingLevel::Low => Some("low"),
            ThinkingLevel::Medium => Some("medium"),
            ThinkingLevel::High => Some("high"),
            ThinkingLevel::XHigh => Some("xhigh"),
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
            ThinkingLevel::Minimal => "Very brief (~5%)",
            ThinkingLevel::Low => "Light (~20%)",
            ThinkingLevel::Medium => "Moderate (~50%)",
            ThinkingLevel::High => "Deep (~80%)",
            ThinkingLevel::XHigh => "Very deep (~95%)",
        }
    }

    /// Returns the short display name for this level.
    pub fn display_name(&self) -> &'static str {
        match self {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::XHigh => "xhigh",
        }
    }

    /// Returns all thinking levels for iteration (e.g., in picker).
    pub fn all() -> &'static [ThinkingLevel] {
        &[
            ThinkingLevel::Off,
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::XHigh,
        ]
    }

    /// Computes the reasoning budget in tokens based on effort percent and `max_tokens`.
    ///
    /// Uses min 1024 tokens to ensure meaningful reasoning.
    /// Returns None if thinking is Off.
    pub fn compute_reasoning_budget(
        &self,
        max_tokens: u32,
        model_output_limit: Option<u32>,
    ) -> Option<u32> {
        const MIN_BUDGET: u32 = 1024;

        let percent = self.effort_percent()?;

        let base = match model_output_limit {
            Some(limit) if limit > 0 => max_tokens.min(limit),
            _ => max_tokens,
        };

        let raw_budget =
            u32::try_from(u64::from(base) * u64::from(percent) / 100).unwrap_or(u32::MAX);

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
