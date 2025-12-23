//! TUI application state.
//!
//! This module contains all TUI state, separate from terminal ownership.
//! This separation allows `view()` to borrow state without conflicting
//! with `terminal.draw()`.

use std::path::PathBuf;

use anyhow::Result;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tui_textarea::TextArea;

use crate::config::Config;
use crate::core::engine::EngineOptions;
use crate::core::session::Session;
use crate::models::AVAILABLE_MODELS;
use crate::providers::anthropic::ChatMessage;
use crate::ui::commands::SLASH_COMMANDS;
use crate::ui::transcript::HistoryCell;

// ============================================================================
// Login State
// ============================================================================

/// Events for the login flow (reducer pattern).
///
/// These events drive the login overlay state machine.
#[derive(Debug, Clone)]
pub enum LoginEvent {
    /// User requested login (e.g., via `/login` command).
    LoginRequested,
    /// User entered the auth code.
    AuthCodeEntered { code: String },
    /// Login succeeded.
    LoginSucceeded,
    /// Login failed with an error message.
    LoginFailed { message: String },
    /// User cancelled the login flow.
    LoginCancelled,
}

/// State for the login overlay.
#[derive(Debug, Clone)]
pub enum LoginState {
    /// Not in login flow.
    Idle,
    /// Showing auth URL, waiting for user to paste code.
    AwaitingCode {
        /// The auth URL to display.
        url: String,
        /// PKCE verifier for code exchange.
        pkce_verifier: String,
        /// User's input (the auth code).
        input: String,
        /// Error message from previous attempt (if any).
        error: Option<String>,
    },
    /// Exchanging code for tokens (async operation in progress).
    Exchanging {
        /// The auth code being exchanged.
        code: String,
        /// PKCE verifier for exchange.
        pkce_verifier: String,
    },
}

impl LoginState {
    /// Returns true if the login overlay should be displayed.
    pub fn is_active(&self) -> bool {
        !matches!(self, LoginState::Idle)
    }
}

// ============================================================================
// Model Picker State
// ============================================================================

/// State for the model picker overlay.
#[derive(Debug, Clone)]
pub struct ModelPickerState {
    /// Currently selected index.
    pub selected: usize,
}

impl ModelPickerState {
    /// Creates a new picker state, selecting the current model if found.
    pub fn new(current_model: &str) -> Self {
        let selected = AVAILABLE_MODELS
            .iter()
            .position(|m| m.id == current_model)
            .unwrap_or(0);
        Self { selected }
    }
}

// ============================================================================
// Command Palette State
// ============================================================================

/// State for the slash command palette.
#[derive(Debug, Clone)]
pub struct CommandPaletteState {
    /// Filter text (characters typed after `/`).
    pub filter: String,
    /// Currently selected command index (into filtered list).
    pub selected: usize,
    /// Whether to insert "/" on Escape (true if opened via "/", false if via Ctrl+P).
    pub insert_slash_on_escape: bool,
}

impl CommandPaletteState {
    /// Creates a new palette state with empty filter.
    pub fn new(insert_slash_on_escape: bool) -> Self {
        Self {
            filter: String::new(),
            selected: 0,
            insert_slash_on_escape,
        }
    }

    /// Returns commands matching the current filter.
    pub fn filtered_commands(&self) -> Vec<&'static crate::ui::commands::SlashCommand> {
        if self.filter.is_empty() {
            SLASH_COMMANDS.iter().collect()
        } else {
            SLASH_COMMANDS
                .iter()
                .filter(|cmd| cmd.matches(&self.filter))
                .collect()
        }
    }

    /// Clamps the selected index to valid range for current filter.
    pub fn clamp_selection(&mut self) {
        let count = self.filtered_commands().len();
        if count == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(count - 1);
        }
    }
}

// ============================================================================
// Scroll Mode
// ============================================================================

/// Scroll mode for the transcript pane.
#[derive(Debug, Clone)]
pub enum ScrollMode {
    /// Auto-scroll to show latest content (bottom of transcript).
    FollowLatest,
    /// User scrolled manually; offset is line index from top.
    Anchored { offset: usize },
}

// ============================================================================
// Engine State
// ============================================================================

/// Engine execution state.
#[derive(Debug)]
pub enum EngineState {
    /// No engine task running, ready for input.
    Idle,
    /// Streaming response in progress.
    Streaming {
        /// Handle to the spawned engine task.
        handle: JoinHandle<Result<(String, Vec<ChatMessage>)>>,
        /// Receiver for engine events.
        rx: mpsc::Receiver<std::sync::Arc<crate::core::events::EngineEvent>>,
        /// ID of the streaming assistant cell in transcript.
        cell_id: crate::ui::transcript::CellId,
        /// Buffered delta text to apply on next tick (coalescing).
        pending_delta: String,
    },
    /// Waiting for first response (shows "thinking...").
    Waiting {
        /// Handle to the spawned engine task.
        handle: JoinHandle<Result<(String, Vec<ChatMessage>)>>,
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
    /// Scroll mode for transcript.
    pub scroll_mode: ScrollMode,
    /// Cached total line count from last render (for scroll calculations).
    pub cached_line_count: usize,
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
    /// Command palette state (None = closed).
    pub command_palette: Option<CommandPaletteState>,
    /// Model picker state (None = closed).
    pub model_picker: Option<ModelPickerState>,
    /// Login overlay state.
    pub login_state: LoginState,
    /// Receiver for async login token exchange result.
    pub login_exchange_rx: Option<mpsc::Receiver<Result<(), String>>>,
    /// Current auth type indicator (cached, refreshed on login/logout).
    pub auth_type: AuthType,
}

impl TuiState {
    /// Creates a new TuiState.
    #[allow(dead_code)] // Convenience constructor for future use
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
            scroll_mode: ScrollMode::FollowLatest,
            cached_line_count: 0,
            session,
            command_history,
            history_index: None,
            input_draft: None,
            spinner_frame: 0,
            command_palette: None,
            model_picker: None,
            login_state: LoginState::Idle,
            login_exchange_rx: None,
            auth_type: AuthType::detect(),
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

    #[test]
    fn test_palette_state_filtered_commands_empty_filter() {
        let state = CommandPaletteState::new(true);
        let filtered = state.filtered_commands();
        assert_eq!(filtered.len(), SLASH_COMMANDS.len());
    }

    #[test]
    fn test_palette_state_filtered_commands_with_filter() {
        let mut state = CommandPaletteState::new(true);
        state.filter = "ne".to_string();
        let filtered = state.filtered_commands();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "new");
    }

    #[test]
    fn test_palette_state_filtered_commands_no_match() {
        let mut state = CommandPaletteState::new(true);
        state.filter = "xyz".to_string();
        let filtered = state.filtered_commands();
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_palette_state_clamp_selection() {
        let mut state = CommandPaletteState::new(true);
        state.selected = 10; // way out of bounds
        state.clamp_selection();
        assert_eq!(state.selected, SLASH_COMMANDS.len() - 1);
    }

    #[test]
    fn test_palette_state_clamp_selection_empty_filter() {
        let mut state = CommandPaletteState::new(true);
        state.filter = "xyz".to_string(); // no matches
        state.selected = 5;
        state.clamp_selection();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_login_state_is_active() {
        assert!(!LoginState::Idle.is_active());
        assert!(
            LoginState::AwaitingCode {
                url: String::new(),
                pkce_verifier: String::new(),
                input: String::new(),
                error: None,
            }
            .is_active()
        );
        assert!(
            LoginState::Exchanging {
                code: String::new(),
                pkce_verifier: String::new(),
            }
            .is_active()
        );
    }
}
