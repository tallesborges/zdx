//! Session and conversation state.
//!
//! Manages the active session, message history, and token usage tracking.

/// Session and conversation state.
///
/// Encapsulates the active session, message history, and usage tracking.
pub struct SessionState {
    /// Active session for persistence (if enabled).
    pub session: Option<crate::core::session::Session>,

    /// Conversation messages (API format).
    pub messages: Vec<crate::providers::anthropic::ChatMessage>,

    /// Cumulative token usage for this session.
    pub usage: SessionUsage,
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionState {
    /// Creates a new SessionState with no active session.
    pub fn new() -> Self {
        Self {
            session: None,
            messages: Vec::new(),
            usage: SessionUsage::new(),
        }
    }

    /// Creates a SessionState with an active session and message history.
    pub fn with_session(
        session: Option<crate::core::session::Session>,
        messages: Vec<crate::providers::anthropic::ChatMessage>,
    ) -> Self {
        Self {
            session,
            messages,
            usage: SessionUsage::new(),
        }
    }

    /// Resets conversation state, clearing messages and usage.
    ///
    /// Note: This does NOT clear the session handle. To start a fresh session,
    /// also set `session = None` or create a new SessionState.
    pub fn reset(&mut self) {
        self.messages.clear();
        self.usage = SessionUsage::new();
    }
}

/// Token usage for the current session.
///
/// Tracks both cumulative tokens (for cost calculation) and latest request
/// tokens (for context window percentage).
///
/// The distinction matters because each API request's `input_tokens` already
/// includes all previous conversation history. Summing across requests would
/// double-count, but we need cumulative totals for accurate cost calculation.
#[derive(Debug, Clone, Default)]
pub struct SessionUsage {
    // ========================================================================
    // Cumulative totals (for cost calculation and token breakdown display)
    // ========================================================================
    /// Total input tokens (non-cached) across all requests
    pub input_tokens: u64,
    /// Total output tokens across all requests
    pub output_tokens: u64,
    /// Total tokens read from cache across all requests
    pub cache_read_tokens: u64,
    /// Total tokens written to cache across all requests
    pub cache_write_tokens: u64,

    // ========================================================================
    // Latest request (for context window percentage)
    // ========================================================================
    /// Total input tokens from latest request (input + cache_read + cache_write)
    latest_input: u64,
    /// Output tokens from latest request
    latest_output: u64,
}

impl SessionUsage {
    /// Creates a new empty SessionUsage.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds usage from a single API response.
    ///
    /// Updates both cumulative totals (for cost) and latest values (for context %).
    ///
    /// Note: Usage updates for a single API request come in two parts:
    /// 1. MessageStart: input_tokens, cache_read, cache_write (output_tokens=0)
    /// 2. MessageDelta: output_tokens (other fields=0)
    ///
    /// We accumulate the latest values to handle split updates correctly.
    pub fn add(&mut self, input: u64, output: u64, cache_read: u64, cache_write: u64) {
        // Cumulative totals for cost calculation
        self.input_tokens += input;
        self.output_tokens += output;
        self.cache_read_tokens += cache_read;
        self.cache_write_tokens += cache_write;

        // Latest request for context window calculation
        // Accumulate (don't replace) to handle split updates from MessageStart + MessageDelta
        if input > 0 || cache_read > 0 || cache_write > 0 {
            // This is a new request (MessageStart) - reset and set input
            self.latest_input = input + cache_read + cache_write;
            self.latest_output = 0; // Will be updated by MessageDelta
        }
        if output > 0 {
            // This is the output update (MessageDelta) - add to latest
            self.latest_output += output;
        }
    }

    /// Context tokens for the latest request (for context window percentage).
    ///
    /// Per Anthropic's documentation, the context window validation is:
    /// > "if the sum of prompt tokens and output tokens exceeds the model's
    /// > context window, the system will return a validation error"
    ///
    /// This applies to a single request, not cumulative across turns.
    /// Each request's `input_tokens` already includes all previous conversation
    /// history, so we only need the latest request's tokens.
    ///
    /// Source: https://docs.anthropic.com/en/docs/build-with-claude/context-windows
    pub fn context_tokens(&self) -> u64 {
        self.latest_input + self.latest_output
    }

    /// Total cumulative tokens across all requests (for display/debugging).
    ///
    /// Note: This is NOT the context window usage. Use `context_tokens()` for that.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    /// Calculates the percentage of context window used.
    ///
    /// Uses all tokens (input + output + cache) per Anthropic's documentation.
    pub fn context_percentage(&self, context_limit: u64) -> f64 {
        if context_limit == 0 {
            return 0.0;
        }
        (self.context_tokens() as f64 / context_limit as f64) * 100.0
    }

    /// Calculates the total cost for this session in USD.
    ///
    /// Uses the pricing from the model (prices are per million tokens).
    pub fn calculate_cost(&self, pricing: &crate::models::ModelPricing) -> f64 {
        let million = 1_000_000.0;

        let input_cost = (self.input_tokens as f64 / million) * pricing.input;
        let output_cost = (self.output_tokens as f64 / million) * pricing.output;
        let cache_read_cost = (self.cache_read_tokens as f64 / million) * pricing.cache_read;
        let cache_write_cost = (self.cache_write_tokens as f64 / million) * pricing.cache_write;

        input_cost + output_cost + cache_read_cost + cache_write_cost
    }

    /// Calculates the cost savings from cache hits.
    ///
    /// Returns the amount saved by using cache_read instead of regular input pricing.
    pub fn cache_savings(&self, pricing: &crate::models::ModelPricing) -> f64 {
        let million = 1_000_000.0;

        // Savings = what we would have paid at input price - what we actually paid at cache_read price
        let would_have_paid = (self.cache_read_tokens as f64 / million) * pricing.input;
        let actually_paid = (self.cache_read_tokens as f64 / million) * pricing.cache_read;

        would_have_paid - actually_paid
    }

    /// Formats token count for display (e.g., "12.5k" or "1.2M").
    pub fn format_tokens(count: u64) -> String {
        if count >= 1_000_000 {
            format!("{:.1}M", count as f64 / 1_000_000.0)
        } else if count >= 1_000 {
            format!("{:.1}k", count as f64 / 1_000.0)
        } else {
            count.to_string()
        }
    }

    /// Formats a context limit for display (e.g., "200k").
    pub fn format_context_limit(limit: u64) -> String {
        if limit >= 1_000_000 {
            format!("{:.0}M", limit as f64 / 1_000_000.0)
        } else if limit >= 1_000 {
            format!("{:.0}k", limit as f64 / 1_000.0)
        } else {
            limit.to_string()
        }
    }

    /// Formats a cost for display (e.g., "$0.008").
    pub fn format_cost(cost: f64) -> String {
        if cost < 0.001 {
            format!("${:.4}", cost)
        } else if cost < 0.01 {
            format!("${:.3}", cost)
        } else {
            format!("${:.2}", cost)
        }
    }
}
