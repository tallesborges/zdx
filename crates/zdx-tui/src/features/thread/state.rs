//! Thread state.
//!
//! Manages the active thread, message history, and token usage tracking.

use zdx_core::core::thread_log::{ThreadLog, Usage};
use zdx_core::models::ModelPricing;
use zdx_core::providers::ChatMessage;

use crate::mutations::ThreadMutation;

/// Thread state.
///
/// Encapsulates the active thread, message history, and usage tracking.
pub struct ThreadState {
    /// Active thread for persistence (if enabled).
    pub thread_log: Option<ThreadLog>,

    /// Cached thread title (if known).
    pub title: Option<String>,

    /// Thread messages (API format).
    pub messages: Vec<ChatMessage>,

    /// Cumulative token usage for this thread.
    pub usage: ThreadUsage,
}

impl Default for ThreadState {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadState {
    /// Creates a new ThreadState with no active thread.
    pub fn new() -> Self {
        Self {
            thread_log: None,
            title: None,
            messages: Vec::new(),
            usage: ThreadUsage::new(),
        }
    }

    /// Creates a ThreadState with an active thread and message history.
    pub fn with_thread(thread_log: Option<ThreadLog>, messages: Vec<ChatMessage>) -> Self {
        let title = thread_log
            .as_ref()
            .and_then(|log| zdx_core::core::thread_log::read_thread_title(&log.id).ok())
            .flatten();
        Self {
            thread_log,
            title,
            messages,
            usage: ThreadUsage::new(),
        }
    }

    /// Applies a cross-slice thread mutation.
    pub fn apply(&mut self, mutation: ThreadMutation) {
        match mutation {
            ThreadMutation::ClearMessages => self.messages.clear(),
            ThreadMutation::SetMessages(messages) => self.messages = messages,
            ThreadMutation::AppendMessage(message) => self.messages.push(message),
            ThreadMutation::SetThread(thread_log) => {
                self.thread_log = thread_log;
                if self.thread_log.is_none() {
                    self.title = None;
                }
            }
            ThreadMutation::ResetUsage => self.usage = ThreadUsage::new(),
            ThreadMutation::SetUsage { cumulative, latest } => {
                self.usage = ThreadUsage::new();
                self.usage.restore(cumulative, latest);
            }
            ThreadMutation::SetTitle(title) => self.title = title,
            ThreadMutation::UpdateUsage {
                input,
                output,
                cache_read,
                cache_write,
            } => self.usage.add(input, output, cache_read, cache_write),
        }
    }
}

/// Token usage for the current thread.
///
/// Tracks both cumulative tokens (for cost calculation) and latest request
/// tokens (for context window percentage).
///
/// The distinction matters because each API request's `input_tokens` already
/// includes all previous thread history. Summing across requests would
/// double-count, but we need cumulative totals for accurate cost calculation.
#[derive(Debug, Clone, Default)]
pub struct ThreadUsage {
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
    // Current turn (for persistence as per-turn deltas)
    // ========================================================================
    /// Input tokens for current turn (reset on new turn)
    turn_input: u64,
    /// Output tokens for current turn (accumulated during turn)
    turn_output: u64,
    /// Cache read tokens for current turn
    turn_cache_read: u64,
    /// Cache write tokens for current turn
    turn_cache_write: u64,

    // ========================================================================
    // Latest request (for context window percentage)
    // ========================================================================
    /// Total input tokens from latest request (input + cache_read + cache_write)
    latest_input: u64,
    /// Output tokens from latest request
    latest_output: u64,

    // ========================================================================
    // Save tracking (to ensure interrupted requests are persisted)
    // ========================================================================
    /// Whether the current request's usage has been saved to disk.
    /// Set to false when a new request starts (input tokens arrive).
    /// Set to true when usage is saved via `mark_saved()`.
    request_saved: bool,
}

impl ThreadUsage {
    /// Creates a new empty ThreadUsage.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds usage from a single API response.
    ///
    /// Updates cumulative totals (for cost), turn values (for persistence),
    /// and latest values (for context %).
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

            // Reset turn tracking for new turn
            self.turn_input = input;
            self.turn_output = 0;
            self.turn_cache_read = cache_read;
            self.turn_cache_write = cache_write;

            // Mark as unsaved - this request needs to be persisted
            self.request_saved = false;
        }
        if output > 0 {
            // This is the output update (MessageDelta) - add to latest
            self.latest_output += output;
            self.turn_output += output;
        }
    }

    /// Returns the current turn's usage values for persistence.
    ///
    /// These are per-request deltas (not cumulative) for event-sourcing style storage.
    pub fn turn_usage(&self) -> Usage {
        Usage::new(
            self.turn_input,
            self.turn_output,
            self.turn_cache_read,
            self.turn_cache_write,
        )
    }

    /// Marks the current request's usage as saved.
    ///
    /// Called after persisting usage to disk. Resets turn values and sets
    /// the saved flag to prevent duplicate saves.
    pub fn mark_saved(&mut self) {
        self.request_saved = true;
        // Reset turn values since they've been persisted
        self.turn_input = 0;
        self.turn_output = 0;
        self.turn_cache_read = 0;
        self.turn_cache_write = 0;
    }

    /// Returns true if there's unsaved usage that should be persisted.
    ///
    /// This handles the case where a request is interrupted before output
    /// tokens arrive - we still want to save the input tokens that were consumed.
    pub fn has_unsaved_usage(&self) -> bool {
        !self.request_saved && self.turn_usage().total() > 0
    }

    /// Restores usage state from persisted thread data.
    ///
    /// Called when loading a thread. Sets both cumulative totals (for cost display)
    /// and latest values (for context % display).
    pub fn restore(&mut self, cumulative: Usage, latest: Usage) {
        // Set cumulative totals
        self.input_tokens = cumulative.input;
        self.output_tokens = cumulative.output;
        self.cache_read_tokens = cumulative.cache_read;
        self.cache_write_tokens = cumulative.cache_write;

        // Set latest for context window calculation
        self.latest_input = latest.context_input();
        self.latest_output = latest.output;

        // Turn values are not needed after restore (no pending save)
        self.turn_input = 0;
        self.turn_output = 0;
        self.turn_cache_read = 0;
        self.turn_cache_write = 0;
    }

    /// Context tokens for the latest request (for context window percentage).
    ///
    /// Per Anthropic's documentation, the context window validation is:
    /// > "if the sum of prompt tokens and output tokens exceeds the model's
    /// > context window, the system will return a validation error"
    ///
    /// This applies to a single request, not cumulative across turns.
    /// Each request's `input_tokens` already includes all previous thread
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

    /// Calculates the total cost for this thread in USD.
    ///
    /// Uses the pricing from the model (prices are per million tokens).
    pub fn calculate_cost(&self, pricing: &ModelPricing) -> f64 {
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
    pub fn cache_savings(&self, pricing: &ModelPricing) -> f64 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_usage_default() {
        let usage = ThreadUsage::new();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_write_tokens, 0);
        assert_eq!(usage.context_tokens(), 0);
    }

    #[test]
    fn test_thread_usage_add() {
        let mut usage = ThreadUsage::new();
        // Simulate split update: MessageStart first (input, cache, no output)
        usage.add(100, 0, 200, 25);
        // Cumulative values (partial)
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 200);
        assert_eq!(usage.cache_write_tokens, 25);
        // Latest context = input + cache_read + cache_write (no output yet)
        assert_eq!(usage.context_tokens(), 325);

        // Simulate MessageDelta (output only)
        usage.add(0, 50, 0, 0);
        // Cumulative values now complete
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        // Latest context now includes output: 325 + 50 = 375
        assert_eq!(usage.context_tokens(), 375);

        // Add more (simulates second turn with split updates)
        usage.add(50, 0, 100, 10); // MessageStart
        // Cumulative values update
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 50); // Not updated yet
        assert_eq!(usage.cache_read_tokens, 300);
        assert_eq!(usage.cache_write_tokens, 35);
        // Latest context = only the second request input: 50 + 100 + 10 = 160 (no output yet)
        assert_eq!(usage.context_tokens(), 160);

        usage.add(0, 25, 0, 0); // MessageDelta
        // Latest context now includes second request output: 160 + 25 = 185
        assert_eq!(usage.context_tokens(), 185);
    }

    #[test]
    fn test_thread_usage_total_tokens() {
        let mut usage = ThreadUsage::new();
        usage.add(1000, 500, 2000, 100);
        // total_tokens = cumulative sum (for cost display)
        assert_eq!(usage.total_tokens(), 3600);
    }

    #[test]
    fn test_thread_usage_context_tokens_uses_latest() {
        let mut usage = ThreadUsage::new();

        // Turn 1: system=1000 (cache_write), user=100 (input)
        // Simulate MessageStart
        usage.add(100, 0, 0, 1000);
        // Context = 100 + 0 + 1000 (no output yet)
        assert_eq!(usage.context_tokens(), 1100);

        // Simulate MessageDelta with output=500
        usage.add(0, 500, 0, 0);
        // Context = 100 + 0 + 1000 + 500 = 1600
        assert_eq!(usage.context_tokens(), 1600);

        // Turn 2: system=1000 (cache_read), prev_conv=600 (input), new_user=100 (input)
        // API reports: input=700, cache_read=1000, cache_write=0
        // Simulate MessageStart
        usage.add(700, 0, 1000, 0);
        // Context = 700 + 1000 + 0 (no output yet) = 1700
        assert_eq!(usage.context_tokens(), 1700);

        // Simulate MessageDelta with output=400
        usage.add(0, 400, 0, 0);
        // Context = 700 + 1000 + 0 + 400 = 2100 (NOT cumulative!)
        assert_eq!(usage.context_tokens(), 2100);

        // But cumulative total is still correct for cost
        // Turn 1: 100 + 500 + 0 + 1000 = 1600
        // Turn 2: 700 + 400 + 1000 + 0 = 2100
        // Total: 3700
        assert_eq!(usage.total_tokens(), 3700);
    }

    #[test]
    fn test_thread_usage_context_percentage() {
        let mut usage = ThreadUsage::new();
        // Latest request: input=10000, cache_read=4000, cache_write=1000, output=5000
        // Context = 10000 + 4000 + 1000 + 5000 = 20000
        usage.add(10000, 5000, 4000, 1000);
        // context_limit = 200000 -> 10%
        let pct = usage.context_percentage(200000);
        assert!((pct - 10.0).abs() < 0.001);
    }

    #[test]
    fn test_thread_usage_context_percentage_zero_limit() {
        let usage = ThreadUsage::new();
        assert_eq!(usage.context_percentage(0), 0.0);
    }

    #[test]
    fn test_thread_usage_calculate_cost() {
        use zdx_core::models::ModelPricing;
        let pricing = ModelPricing {
            input: 3.0,        // $3 per million
            output: 15.0,      // $15 per million
            cache_read: 0.3,   // $0.30 per million
            cache_write: 3.75, // $3.75 per million
        };

        let mut usage = ThreadUsage::new();
        // 1 million tokens of each type
        usage.add(1_000_000, 1_000_000, 1_000_000, 1_000_000);

        let cost = usage.calculate_cost(&pricing);
        // Expected: 3 + 15 + 0.3 + 3.75 = 22.05
        assert!((cost - 22.05).abs() < 0.001);
    }

    #[test]
    fn test_thread_usage_cache_savings() {
        use zdx_core::models::ModelPricing;
        let pricing = ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        };

        let mut usage = ThreadUsage::new();
        // 1 million cache_read tokens
        usage.add(0, 0, 1_000_000, 0);

        let savings = usage.cache_savings(&pricing);
        // Savings = (1M * $3/M) - (1M * $0.3/M) = 3.0 - 0.3 = 2.7
        assert!((savings - 2.7).abs() < 0.001);
    }

    #[test]
    fn test_thread_usage_format_tokens() {
        assert_eq!(ThreadUsage::format_tokens(500), "500");
        assert_eq!(ThreadUsage::format_tokens(1500), "1.5k");
        assert_eq!(ThreadUsage::format_tokens(1_500_000), "1.5M");
    }

    #[test]
    fn test_thread_usage_format_context_limit() {
        assert_eq!(ThreadUsage::format_context_limit(500), "500");
        assert_eq!(ThreadUsage::format_context_limit(200_000), "200k");
        assert_eq!(ThreadUsage::format_context_limit(1_000_000), "1M");
    }

    #[test]
    fn test_thread_usage_format_cost() {
        assert_eq!(ThreadUsage::format_cost(0.0001), "$0.0001");
        assert_eq!(ThreadUsage::format_cost(0.008), "$0.008");
        assert_eq!(ThreadUsage::format_cost(0.15), "$0.15");
        assert_eq!(ThreadUsage::format_cost(1.50), "$1.50");
    }

    #[test]
    fn test_thread_usage_has_unsaved_usage_after_input() {
        let mut usage = ThreadUsage::new();
        // Initially no unsaved usage
        assert!(!usage.has_unsaved_usage());

        // After receiving input tokens (MessageStart), we have unsaved usage
        usage.add(1000, 0, 500, 100);
        assert!(usage.has_unsaved_usage());

        // After receiving output tokens (MessageDelta), still unsaved
        usage.add(0, 200, 0, 0);
        assert!(usage.has_unsaved_usage());
    }

    #[test]
    fn test_thread_usage_mark_saved_clears_unsaved() {
        let mut usage = ThreadUsage::new();
        usage.add(1000, 0, 500, 100);
        assert!(usage.has_unsaved_usage());

        // After marking saved, no unsaved usage
        usage.mark_saved();
        assert!(!usage.has_unsaved_usage());

        // turn_usage should be zeroed
        let turn = usage.turn_usage();
        assert_eq!(turn.total(), 0);
    }

    #[test]
    fn test_thread_usage_new_request_resets_saved_flag() {
        let mut usage = ThreadUsage::new();

        // First request: receive input, mark saved
        usage.add(1000, 0, 0, 0);
        usage.add(0, 200, 0, 0);
        usage.mark_saved();
        assert!(!usage.has_unsaved_usage());

        // Second request: new input tokens should reset saved flag
        usage.add(2000, 0, 0, 0);
        assert!(usage.has_unsaved_usage());
    }

    #[test]
    fn test_thread_usage_interrupted_request_has_unsaved_input() {
        let mut usage = ThreadUsage::new();

        // Simulate interrupted request: input arrives but no output
        usage.add(50000, 0, 10000, 5000);
        // User interrupts before MessageDelta arrives

        // Should still have unsaved usage (the input tokens)
        assert!(usage.has_unsaved_usage());

        let turn = usage.turn_usage();
        assert_eq!(turn.input, 50000);
        assert_eq!(turn.cache_read, 10000);
        assert_eq!(turn.cache_write, 5000);
        assert_eq!(turn.output, 0); // No output because interrupted
        assert_eq!(turn.total(), 65000);
    }
}
