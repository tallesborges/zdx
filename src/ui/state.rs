//! TUI application state.
//!
//! This module contains all TUI state, separate from terminal ownership.
//! This separation allows `view()` to borrow state without conflicting
//! with `terminal.draw()`.

use std::path::PathBuf;

use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders};
use tokio::sync::mpsc;
use tui_textarea::TextArea;

use crate::config::Config;
use crate::core::engine::EngineOptions;
use crate::core::session::Session;
use crate::providers::anthropic::ChatMessage;
// Re-export overlay types for backwards compatibility
pub use crate::ui::overlays::{
    CommandPaletteState, LoginState, ModelPickerState, ThinkingPickerState,
};
use crate::ui::transcript::{HistoryCell, WrapCache};

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

    /// Returns the login state mutably if active.
    #[cfg(test)]
    pub fn as_login_mut(&mut self) -> Option<&mut LoginState> {
        match self {
            OverlayState::Login(l) => Some(l),
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
// Scroll State
// ============================================================================

/// Scroll mode for the transcript pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollMode {
    /// Auto-scroll to show latest content (bottom of transcript).
    FollowLatest,
    /// User scrolled manually; offset is line index from top.
    Anchored { offset: usize },
}

/// Scroll state for the transcript pane.
///
/// Encapsulates scroll mode, cached line count, and all scroll navigation logic.
/// This keeps scroll math in one place and simplifies the reducer.
#[derive(Debug, Clone)]
pub struct ScrollState {
    /// Current scroll mode (follow latest or anchored at offset).
    pub mode: ScrollMode,
    /// Cached total line count from last render (for scroll calculations).
    pub cached_line_count: usize,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            mode: ScrollMode::FollowLatest,
            cached_line_count: 0,
        }
    }
}

impl ScrollState {
    /// Creates a new ScrollState in follow mode.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true if currently following output (auto-scroll).
    pub fn is_following(&self) -> bool {
        matches!(self.mode, ScrollMode::FollowLatest)
    }

    /// Returns the current scroll offset for rendering.
    ///
    /// In FollowLatest mode, calculates offset to show bottom of content.
    /// In Anchored mode, returns the stored offset (clamped to valid range).
    pub fn get_offset(&self, viewport_height: usize) -> usize {
        match &self.mode {
            ScrollMode::FollowLatest => self.cached_line_count.saturating_sub(viewport_height),
            ScrollMode::Anchored { offset } => {
                let max_offset = self.cached_line_count.saturating_sub(viewport_height);
                (*offset).min(max_offset)
            }
        }
    }

    /// Returns true if there's content below the current viewport.
    #[cfg(test)]
    pub fn has_content_below(&self, viewport_height: usize) -> bool {
        let offset = self.get_offset(viewport_height);
        offset + viewport_height < self.cached_line_count
    }

    /// Scrolls up by the given number of lines.
    pub fn scroll_up(&mut self, lines: usize, viewport_height: usize) {
        let current_offset = self.get_offset(viewport_height);
        let new_offset = current_offset.saturating_sub(lines);
        self.mode = ScrollMode::Anchored { offset: new_offset };
    }

    /// Scrolls down by the given number of lines.
    ///
    /// Transitions to FollowLatest mode when reaching the bottom.
    pub fn scroll_down(&mut self, lines: usize, viewport_height: usize) {
        if matches!(self.mode, ScrollMode::FollowLatest) {
            return; // Already at bottom
        }

        let current_offset = self.get_offset(viewport_height);
        let max_offset = self.cached_line_count.saturating_sub(viewport_height);
        let new_offset = (current_offset + lines).min(max_offset);

        if new_offset >= max_offset {
            self.mode = ScrollMode::FollowLatest;
        } else {
            self.mode = ScrollMode::Anchored { offset: new_offset };
        }
    }

    /// Scrolls to the top of the transcript.
    pub fn scroll_to_top(&mut self) {
        self.mode = ScrollMode::Anchored { offset: 0 };
    }

    /// Scrolls to the bottom of the transcript (enables follow mode).
    pub fn scroll_to_bottom(&mut self) {
        self.mode = ScrollMode::FollowLatest;
    }

    /// Scrolls up by one page.
    pub fn page_up(&mut self, viewport_height: usize) {
        self.scroll_up(viewport_height.max(1), viewport_height);
    }

    /// Scrolls down by one page.
    pub fn page_down(&mut self, viewport_height: usize) {
        self.scroll_down(viewport_height.max(1), viewport_height);
    }

    /// Updates the cached line count.
    ///
    /// Call this after rendering to keep scroll calculations accurate.
    pub fn update_line_count(&mut self, line_count: usize) {
        self.cached_line_count = line_count;
    }

    /// Resets scroll state to follow mode (e.g., after clearing transcript).
    pub fn reset(&mut self) {
        self.mode = ScrollMode::FollowLatest;
        self.cached_line_count = 0;
    }
}

// ============================================================================
// Engine State
// ============================================================================

/// Engine execution state.
///
/// Tracks the current engine task and its event channel.
/// The task sends events through the channel, including `TurnComplete` when done.
#[derive(Debug)]
pub enum EngineState {
    /// No engine task running, ready for input.
    Idle,
    /// Streaming response in progress.
    Streaming {
        /// Receiver for engine events.
        rx: mpsc::Receiver<std::sync::Arc<crate::core::events::EngineEvent>>,
        /// ID of the streaming assistant cell in transcript.
        cell_id: crate::ui::transcript::CellId,
        /// Buffered delta text to apply on next tick (coalescing).
        pending_delta: String,
    },
    /// Waiting for first response.
    Waiting {
        /// Receiver for engine events.
        rx: mpsc::Receiver<std::sync::Arc<crate::core::events::EngineEvent>>,
    },
}

impl EngineState {
    /// Returns true if the engine is currently running (waiting or streaming).
    pub fn is_running(&self) -> bool {
        !matches!(self, EngineState::Idle)
    }
}

// ============================================================================
// Auth Type
// ============================================================================

/// Authentication type indicator for status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    /// Using OAuth token from ~/.zdx/oauth.json
    OAuth,
    /// Using API key from environment
    ApiKey,
    /// No authentication configured
    None,
}

impl AuthType {
    /// Detects the current authentication type.
    pub fn detect() -> Self {
        use crate::providers::oauth::anthropic;

        // Check for OAuth credentials first
        if let Ok(Some(_creds)) = anthropic::load_credentials() {
            return AuthType::OAuth;
        }

        // Check for API key in environment
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            return AuthType::ApiKey;
        }

        AuthType::None
    }
}

// ============================================================================
// Session Usage
// ============================================================================

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

// ============================================================================
// TuiState
// ============================================================================

/// TUI application state.
///
/// Contains all state for the TUI, separate from terminal ownership.
/// This separation allows pure rendering without borrow conflicts.
pub struct TuiState {
    /// Flag indicating the app should quit.
    pub should_quit: bool,
    /// Text area for input.
    pub textarea: TextArea<'static>,
    /// Transcript cells (in-memory display).
    pub transcript: Vec<HistoryCell>,
    /// Engine configuration.
    pub config: Config,
    /// Engine options (root path, etc).
    pub engine_opts: EngineOptions,
    /// System prompt for the engine.
    pub system_prompt: Option<String>,
    /// Message history for the engine.
    pub messages: Vec<ChatMessage>,
    /// Current engine state.
    pub engine_state: EngineState,
    /// Scroll state for transcript (mode, offset, cached line count).
    pub scroll: ScrollState,
    /// Session for persistence (if enabled).
    pub session: Option<Session>,
    /// Command history for ↑/↓ navigation.
    pub command_history: Vec<String>,
    /// Current position in command history (None = not navigating).
    pub history_index: Option<usize>,
    /// Draft text saved when navigating history.
    pub input_draft: Option<String>,
    /// Spinner animation frame counter (for running tools).
    pub spinner_frame: usize,
    /// Active overlay state (command palette, model picker, or login).
    pub overlay: OverlayState,
    /// Receiver for async login token exchange result.
    pub login_exchange_rx: Option<mpsc::Receiver<Result<(), String>>>,
    /// Current auth type indicator (cached, refreshed on login/logout).
    pub auth_type: AuthType,
    /// Git branch name (cached at startup).
    pub git_branch: Option<String>,
    /// Shortened display path (cached at startup).
    pub display_path: String,
    /// Cache for wrapped line rendering.
    pub wrap_cache: WrapCache,
    /// Cumulative token usage for this session.
    pub usage: SessionUsage,
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
        // Set up textarea with styling
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Input (Enter=send, Shift+Enter=newline, Ctrl+J=newline) "),
        );

        let engine_opts = EngineOptions { root };

        // Cache display values at startup (avoids I/O during render)
        let git_branch = get_git_branch(&engine_opts.root);
        let display_path = shorten_path(&engine_opts.root);

        // Build transcript from history
        let transcript = Self::build_transcript_from_history(&history);

        // Build command history from previous user messages
        let command_history: Vec<String> = transcript
            .iter()
            .filter_map(|cell| {
                if let HistoryCell::User { content, .. } = cell {
                    Some(content.clone())
                } else {
                    None
                }
            })
            .collect();

        Self {
            should_quit: false,
            textarea,
            transcript,
            config,
            engine_opts,
            system_prompt,
            messages: history,
            engine_state: EngineState::Idle,
            scroll: ScrollState::new(),
            session,
            command_history,
            history_index: None,
            input_draft: None,
            spinner_frame: 0,
            overlay: OverlayState::None,
            login_exchange_rx: None,
            auth_type: AuthType::detect(),
            git_branch,
            display_path,
            wrap_cache: WrapCache::new(),
            usage: SessionUsage::new(),
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
        self.auth_type = AuthType::detect();
    }

    /// Gets the current input text.
    pub fn get_input_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Clears the input textarea.
    pub fn clear_input(&mut self) {
        self.textarea.select_all();
        self.textarea.cut();
        self.reset_history_navigation();
    }

    /// Sets the input textarea to the given text.
    pub fn set_input_text(&mut self, text: &str) {
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.insert_str(text);
    }

    /// Resets history navigation state.
    pub fn reset_history_navigation(&mut self) {
        self.history_index = None;
        self.input_draft = None;
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
