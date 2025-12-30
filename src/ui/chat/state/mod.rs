//! TUI application state.
//!
//! This module contains all TUI state, separate from terminal ownership.
//! This separation allows `view()` to borrow state without conflicting
//! with `terminal.draw()`.

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::config::Config;
use crate::core::agent::AgentOptions;
use crate::core::session::Session;
use crate::providers::anthropic::ChatMessage;
use crate::ui::transcript::HistoryCell;

// Module declarations
mod auth;
mod input;
mod session;
mod transcript;

// Re-export types from submodules
pub use auth::{AuthState, AuthStatus};
pub use input::{HandoffState, InputState};
pub use session::{SessionState, SessionUsage};
pub use transcript::TranscriptState;
// Re-export VisibleRange for view.rs
pub use transcript::VisibleRange;
// Re-export scroll types for tests only
#[cfg(test)]
pub use transcript::{ScrollMode, ScrollState};

// Re-export overlay types for backwards compatibility
pub use crate::ui::chat::overlays::{
    CommandPaletteState, LoginState, ModelPickerState, SessionPickerState, ThinkingPickerState,
};

// ============================================================================
// Overlay State (Unified)
// ============================================================================

/// Unified overlay state.
///
/// Only one overlay can be active at a time. This eliminates the cascade of
/// `if palette.is_some() / if picker.is_some() / if login.is_active()` checks.
#[derive(Debug, Clone)]
pub enum OverlayState {
    /// No overlay active.
    None,
    /// Command palette is open.
    CommandPalette(CommandPaletteState),
    /// Model picker is open.
    ModelPicker(ModelPickerState),
    /// Thinking level picker is open.
    ThinkingPicker(ThinkingPickerState),
    /// Session picker is open.
    SessionPicker(SessionPickerState),
    /// Login flow is active.
    Login(LoginState),
}

impl OverlayState {
    /// Returns true if any overlay is active.
    #[cfg(test)]
    pub fn is_active(&self) -> bool {
        !matches!(self, OverlayState::None)
    }

    /// Returns the command palette state if active.
    pub fn as_command_palette(&self) -> Option<&CommandPaletteState> {
        match self {
            OverlayState::CommandPalette(p) => Some(p),
            _ => None,
        }
    }

    /// Returns the command palette state mutably if active.
    pub fn as_command_palette_mut(&mut self) -> Option<&mut CommandPaletteState> {
        match self {
            OverlayState::CommandPalette(p) => Some(p),
            _ => None,
        }
    }

    /// Returns the model picker state if active.
    pub fn as_model_picker(&self) -> Option<&ModelPickerState> {
        match self {
            OverlayState::ModelPicker(p) => Some(p),
            _ => None,
        }
    }

    /// Returns the model picker state mutably if active.
    pub fn as_model_picker_mut(&mut self) -> Option<&mut ModelPickerState> {
        match self {
            OverlayState::ModelPicker(p) => Some(p),
            _ => None,
        }
    }

    /// Returns the thinking picker state if active.
    pub fn as_thinking_picker(&self) -> Option<&ThinkingPickerState> {
        match self {
            OverlayState::ThinkingPicker(p) => Some(p),
            _ => None,
        }
    }

    /// Returns the thinking picker state mutably if active.
    pub fn as_thinking_picker_mut(&mut self) -> Option<&mut ThinkingPickerState> {
        match self {
            OverlayState::ThinkingPicker(p) => Some(p),
            _ => None,
        }
    }

    /// Returns the login state if active.
    #[cfg(test)]
    pub fn as_login(&self) -> Option<&LoginState> {
        match self {
            OverlayState::Login(l) => Some(l),
            _ => None,
        }
    }

    /// Returns the session picker state if active.
    pub fn as_session_picker(&self) -> Option<&SessionPickerState> {
        match self {
            OverlayState::SessionPicker(p) => Some(p),
            _ => None,
        }
    }

    /// Returns the session picker state mutably if active.
    pub fn as_session_picker_mut(&mut self) -> Option<&mut SessionPickerState> {
        match self {
            OverlayState::SessionPicker(p) => Some(p),
            _ => None,
        }
    }
}

// ============================================================================
// Startup Helpers (one-shot I/O, not called during render)
// ============================================================================

/// Gets the current git branch name from .git/HEAD.
fn get_git_branch(root: &std::path::Path) -> Option<String> {
    let head_path = root.join(".git/HEAD");
    if let Ok(content) = std::fs::read_to_string(head_path)
        && let Some(branch) = content.strip_prefix("ref: refs/heads/")
    {
        return Some(branch.trim().to_string());
    }
    None
}

/// Shortens a path for display, using ~ for home directory.
fn shorten_path(path: &std::path::Path) -> String {
    // Canonicalize to resolve "." and ".." to absolute path
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if let Some(home) = dirs::home_dir()
        && let Ok(relative) = path.strip_prefix(&home)
    {
        return format!("~/{}", relative.display());
    }
    path.display().to_string()
}

// ============================================================================
// Agent State
// ============================================================================

/// Agent execution state.
///
/// Tracks the current agent task and its event channel.
/// The task sends events through the channel, including `TurnComplete` when done.
#[derive(Debug)]
pub enum AgentState {
    /// No agent task running, ready for input.
    Idle,
    /// Streaming response in progress.
    Streaming {
        /// Receiver for agent events.
        rx: mpsc::Receiver<std::sync::Arc<crate::core::events::AgentEvent>>,
        /// ID of the streaming assistant cell in transcript.
        cell_id: crate::ui::transcript::CellId,
        /// Buffered delta text to apply on next tick (coalescing).
        pending_delta: String,
    },
    /// Waiting for first response.
    Waiting {
        /// Receiver for agent events.
        rx: mpsc::Receiver<std::sync::Arc<crate::core::events::AgentEvent>>,
    },
}

impl AgentState {
    /// Returns true if the agent is currently running (waiting or streaming).
    pub fn is_running(&self) -> bool {
        !matches!(self, AgentState::Idle)
    }
}

// ============================================================================
// TuiState
// ============================================================================

/// TUI application state.
pub struct TuiState {
    /// Flag indicating the app should quit.
    pub should_quit: bool,
    /// User input state (textarea, history, navigation).
    pub input: InputState,
    /// Transcript display state (cells, scroll, layout, cache).
    pub transcript: TranscriptState,
    /// Session and conversation state (session, messages, usage).
    pub conversation: SessionState,
    /// Authentication state (auth type, login flow).
    pub auth: AuthState,
    /// Agent configuration.
    pub config: Config,
    /// Agent options (root path, etc).
    pub agent_opts: AgentOptions,
    /// System prompt for the agent.
    pub system_prompt: Option<String>,
    /// Current agent state.
    pub agent_state: AgentState,
    /// Spinner animation frame counter (for running tools).
    pub spinner_frame: usize,
    /// Active overlay state (command palette, model picker, or login).
    pub overlay: OverlayState,
    /// Git branch name (cached at startup).
    pub git_branch: Option<String>,
    /// Shortened display path (cached at startup).
    pub display_path: String,
}

impl TuiState {
    /// Creates a new TuiState.
    #[cfg(test)]
    pub fn new(
        config: Config,
        root: PathBuf,
        system_prompt: Option<String>,
        session: Option<Session>,
    ) -> Self {
        Self::with_history(config, root, system_prompt, session, Vec::new())
    }

    /// Creates a TuiState with pre-loaded message history.
    ///
    /// Used for resuming previous sessions.
    pub fn with_history(
        config: Config,
        root: PathBuf,
        system_prompt: Option<String>,
        session: Option<Session>,
        history: Vec<ChatMessage>,
    ) -> Self {
        let agent_opts = AgentOptions { root };

        // Cache display values at startup (avoids I/O during render)
        let git_branch = get_git_branch(&agent_opts.root);
        let display_path = shorten_path(&agent_opts.root);

        // Build transcript from history
        let transcript_cells = Self::build_transcript_from_history(&history);

        // Build command history from previous user messages
        let command_history: Vec<String> = transcript_cells
            .iter()
            .filter_map(|cell| {
                if let HistoryCell::User { content, .. } = cell {
                    Some(content.clone())
                } else {
                    None
                }
            })
            .collect();

        // Create transcript state with history
        let mut transcript = TranscriptState::new();
        transcript.cells = transcript_cells;

        // Create input state with command history
        let mut input = InputState::new();
        input.history = command_history;

        // Create session state with history
        let conversation = SessionState::with_session(session, history);

        // Create auth state
        let auth = AuthState::new();

        Self {
            should_quit: false,
            input,
            transcript,
            conversation,
            auth,
            config,
            agent_opts,
            system_prompt,
            agent_state: AgentState::Idle,
            spinner_frame: 0,
            overlay: OverlayState::None,
            git_branch,
            display_path,
        }
    }

    /// Builds transcript cells from message history.
    fn build_transcript_from_history(messages: &[ChatMessage]) -> Vec<HistoryCell> {
        use crate::providers::anthropic::MessageContent;

        let mut transcript = Vec::new();

        for msg in messages {
            let text = match &msg.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Blocks(blocks) => {
                    // Extract text blocks, ignore tool use/result for display
                    blocks
                        .iter()
                        .filter_map(|b| {
                            if let crate::providers::anthropic::ChatContentBlock::Text(t) = b {
                                Some(t.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            };

            if text.is_empty() {
                continue;
            }

            let cell = match msg.role.as_str() {
                "user" => HistoryCell::user(&text),
                "assistant" => HistoryCell::assistant(&text),
                _ => continue,
            };
            transcript.push(cell);
        }

        transcript
    }

    /// Refreshes the cached auth type (call after login/logout).
    pub fn refresh_auth_type(&mut self) {
        self.auth.refresh();
    }

    /// Gets the current input text.
    pub fn get_input_text(&self) -> String {
        self.input.get_text()
    }

    /// Clears the input textarea.
    pub fn clear_input(&mut self) {
        self.input.clear();
    }

    /// Resets history navigation state.
    pub fn reset_history_navigation(&mut self) {
        self.input.reset_navigation();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    // OverlayState Tests
    // ========================================================================

    #[test]
    fn test_overlay_state_is_active() {
        use crate::config::ThinkingLevel;
        assert!(!OverlayState::None.is_active());
        assert!(OverlayState::CommandPalette(CommandPaletteState::new(true)).is_active());
        assert!(OverlayState::ModelPicker(ModelPickerState::new("test")).is_active());
        assert!(
            OverlayState::ThinkingPicker(ThinkingPickerState::new(ThinkingLevel::Off)).is_active()
        );
        assert!(OverlayState::Login(LoginState::Exchanging).is_active());
    }

    #[test]
    fn test_overlay_state_accessors() {
        use crate::config::ThinkingLevel;
        let palette = OverlayState::CommandPalette(CommandPaletteState::new(true));
        assert!(palette.as_command_palette().is_some());
        assert!(palette.as_model_picker().is_none());
        assert!(palette.as_thinking_picker().is_none());
        assert!(palette.as_login().is_none());

        let picker = OverlayState::ModelPicker(ModelPickerState::new("test"));
        assert!(picker.as_command_palette().is_none());
        assert!(picker.as_model_picker().is_some());

        let thinking_picker =
            OverlayState::ThinkingPicker(ThinkingPickerState::new(ThinkingLevel::Medium));
        assert!(thinking_picker.as_thinking_picker().is_some());
        assert!(thinking_picker.as_model_picker().is_none());

        let login = OverlayState::Login(LoginState::Exchanging);
        assert!(login.as_login().is_some());
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
