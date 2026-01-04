//! TUI application state (re-export hub).
//!
//! This module re-exports state types from their source locations:
//! - `AppState`, `TuiState`, `AgentState` from `app.rs`
//! - Feature state types from their respective feature modules
//!
//! ## State Hierarchy
//!
//! See `app.rs` for the full state hierarchy documentation.
//!
//! ## Split State Architecture
//!
//! State is split between `TuiState` (non-overlay) and `Option<Overlay>`:
//! - `TuiState` contains all non-overlay UI state
//! - `Option<Overlay>` holds the active overlay if any
//! - `AppState` combines both for runtime use
//!
//! This allows overlay handlers to get `&mut self` and `&mut TuiState` simultaneously.

// Module declarations for backward compatibility shims
mod auth;
mod input;
mod session;
mod transcript;

// Re-export from app.rs (core state types)
pub use crate::modes::tui::app::{AgentState, AppState, TuiState};

// Re-export types from feature modules (via shims for backward compat)
// These are intentionally kept for backward compatibility even if unused internally
#[allow(unused_imports)]
pub use auth::{AuthState, AuthStatus};
#[allow(unused_imports)]
pub use input::{HandoffState, InputState};
#[allow(unused_imports)]
pub use session::{SessionOpsState, SessionState, SessionUsage};
#[allow(unused_imports)]
pub use transcript::TranscriptState;

// Re-export scroll types for tests only
#[cfg(test)]
pub use transcript::{ScrollMode, ScrollState};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThinkingLevel;
    use crate::modes::tui::overlays::{
        CommandPaletteState, FilePickerState, LoginState, ModelPickerState, Overlay,
        ThinkingPickerState,
    };

    // ========================================================================
    // ScrollState Tests
    // ========================================================================

    #[test]
    fn test_scroll_state_default() {
        let scroll = ScrollState::default();
        assert!(matches!(scroll.mode, ScrollMode::FollowLatest));
        assert_eq!(scroll.cached_line_count, 0);
        assert!(scroll.is_following());
    }

    #[test]
    fn test_scroll_state_get_offset_follow_mode() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);

        // In follow mode, offset should show the bottom
        let offset = scroll.get_offset(20);
        assert_eq!(offset, 80); // 100 - 20 = 80
    }

    #[test]
    fn test_scroll_state_get_offset_anchored_mode() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 30 };

        let offset = scroll.get_offset(20);
        assert_eq!(offset, 30);
    }

    #[test]
    fn test_scroll_state_get_offset_clamps_to_max() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 95 }; // Too close to bottom

        let offset = scroll.get_offset(20);
        assert_eq!(offset, 80); // max_offset = 100 - 20 = 80
    }

    #[test]
    fn test_scroll_state_scroll_up_from_follow() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);

        scroll.scroll_up(5, 20);

        // Should anchor at line 75 (80 - 5)
        assert!(matches!(scroll.mode, ScrollMode::Anchored { offset: 75 }));
    }

    #[test]
    fn test_scroll_state_scroll_up_clamped_to_zero() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 3 };

        scroll.scroll_up(10, 20); // Would go negative

        assert!(matches!(scroll.mode, ScrollMode::Anchored { offset: 0 }));
    }

    #[test]
    fn test_scroll_state_scroll_down_to_bottom() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 75 };

        scroll.scroll_down(10, 20); // Would exceed max

        // Should transition to FollowLatest
        assert!(matches!(scroll.mode, ScrollMode::FollowLatest));
    }

    #[test]
    fn test_scroll_state_scroll_down_partial() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 50 };

        scroll.scroll_down(10, 20);

        assert!(matches!(scroll.mode, ScrollMode::Anchored { offset: 60 }));
    }

    #[test]
    fn test_scroll_state_scroll_down_noop_when_following() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        assert!(scroll.is_following());

        scroll.scroll_down(10, 20);

        // Should still be following
        assert!(scroll.is_following());
    }

    #[test]
    fn test_scroll_state_scroll_to_top() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);

        scroll.scroll_to_top();

        assert!(matches!(scroll.mode, ScrollMode::Anchored { offset: 0 }));
    }

    #[test]
    fn test_scroll_state_scroll_to_bottom() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 30 };

        scroll.scroll_to_bottom();

        assert!(matches!(scroll.mode, ScrollMode::FollowLatest));
    }

    #[test]
    fn test_scroll_state_page_up() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        // Start at bottom (follow mode, offset = 80)

        scroll.page_up(20);

        // Should move up by viewport_height (20), so 80 - 20 = 60
        assert!(matches!(scroll.mode, ScrollMode::Anchored { offset: 60 }));
    }

    #[test]
    fn test_scroll_state_page_down() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 40 };

        scroll.page_down(20);

        assert!(matches!(scroll.mode, ScrollMode::Anchored { offset: 60 }));
    }

    #[test]
    fn test_scroll_state_has_content_below() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);

        // At top, should have content below
        scroll.mode = ScrollMode::Anchored { offset: 0 };
        assert!(scroll.has_content_below(20));

        // At bottom, should not have content below
        scroll.scroll_to_bottom();
        assert!(!scroll.has_content_below(20));
    }

    #[test]
    fn test_scroll_state_reset() {
        let mut scroll = ScrollState::new();
        scroll.update_line_count(100);
        scroll.mode = ScrollMode::Anchored { offset: 50 };

        scroll.reset();

        assert!(matches!(scroll.mode, ScrollMode::FollowLatest));
        assert_eq!(scroll.cached_line_count, 0);
    }

    // ========================================================================
    // Overlay Tests
    // ========================================================================

    #[test]
    fn test_overlay_is_some() {
        let none: Option<Overlay> = None;
        assert!(none.is_none());

        let (palette, _) = CommandPaletteState::open(true);
        let overlay: Option<Overlay> = Some(Overlay::CommandPalette(palette));
        assert!(overlay.is_some());

        let (picker, _) = ModelPickerState::open("test");
        let overlay: Option<Overlay> = Some(Overlay::ModelPicker(picker));
        assert!(overlay.is_some());

        let (thinking, _) = ThinkingPickerState::open(ThinkingLevel::Off);
        let overlay: Option<Overlay> = Some(Overlay::ThinkingPicker(thinking));
        assert!(overlay.is_some());

        let overlay: Option<Overlay> = Some(Overlay::Login(LoginState::Exchanging));
        assert!(overlay.is_some());

        let (file_picker, _) = FilePickerState::open(0);
        let overlay: Option<Overlay> = Some(Overlay::FilePicker(file_picker));
        assert!(overlay.is_some());
    }

    // ========================================================================
    // SessionUsage Tests
    // ========================================================================

    #[test]
    fn test_session_usage_default() {
        let usage = SessionUsage::new();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_write_tokens, 0);
        assert_eq!(usage.context_tokens(), 0);
    }

    #[test]
    fn test_session_usage_add() {
        let mut usage = SessionUsage::new();
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
    fn test_session_usage_total_tokens() {
        let mut usage = SessionUsage::new();
        usage.add(1000, 500, 2000, 100);
        // total_tokens = cumulative sum (for cost display)
        assert_eq!(usage.total_tokens(), 3600);
    }

    #[test]
    fn test_session_usage_context_tokens_uses_latest() {
        let mut usage = SessionUsage::new();

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
    fn test_session_usage_context_percentage() {
        let mut usage = SessionUsage::new();
        // Latest request: input=10000, cache_read=4000, cache_write=1000, output=5000
        // Context = 10000 + 4000 + 1000 + 5000 = 20000
        usage.add(10000, 5000, 4000, 1000);
        // context_limit = 200000 -> 10%
        let pct = usage.context_percentage(200000);
        assert!((pct - 10.0).abs() < 0.001);
    }

    #[test]
    fn test_session_usage_context_percentage_zero_limit() {
        let usage = SessionUsage::new();
        assert_eq!(usage.context_percentage(0), 0.0);
    }

    #[test]
    fn test_session_usage_calculate_cost() {
        use crate::models::ModelPricing;
        let pricing = ModelPricing {
            input: 3.0,        // $3 per million
            output: 15.0,      // $15 per million
            cache_read: 0.3,   // $0.30 per million
            cache_write: 3.75, // $3.75 per million
        };

        let mut usage = SessionUsage::new();
        // 1 million tokens of each type
        usage.add(1_000_000, 1_000_000, 1_000_000, 1_000_000);

        let cost = usage.calculate_cost(&pricing);
        // Expected: 3 + 15 + 0.3 + 3.75 = 22.05
        assert!((cost - 22.05).abs() < 0.001);
    }

    #[test]
    fn test_session_usage_cache_savings() {
        use crate::models::ModelPricing;
        let pricing = ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        };

        let mut usage = SessionUsage::new();
        // 1 million cache_read tokens
        usage.add(0, 0, 1_000_000, 0);

        let savings = usage.cache_savings(&pricing);
        // Savings = (1M * $3/M) - (1M * $0.3/M) = 3.0 - 0.3 = 2.7
        assert!((savings - 2.7).abs() < 0.001);
    }

    #[test]
    fn test_session_usage_format_tokens() {
        assert_eq!(SessionUsage::format_tokens(500), "500");
        assert_eq!(SessionUsage::format_tokens(1500), "1.5k");
        assert_eq!(SessionUsage::format_tokens(1_500_000), "1.5M");
    }

    #[test]
    fn test_session_usage_format_context_limit() {
        assert_eq!(SessionUsage::format_context_limit(500), "500");
        assert_eq!(SessionUsage::format_context_limit(200_000), "200k");
        assert_eq!(SessionUsage::format_context_limit(1_000_000), "1M");
    }

    #[test]
    fn test_session_usage_format_cost() {
        assert_eq!(SessionUsage::format_cost(0.0001), "$0.0001");
        assert_eq!(SessionUsage::format_cost(0.008), "$0.008");
        assert_eq!(SessionUsage::format_cost(0.15), "$0.15");
        assert_eq!(SessionUsage::format_cost(1.50), "$1.50");
    }
}
