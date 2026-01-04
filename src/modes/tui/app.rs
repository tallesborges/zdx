//! Application state composition.
//!
//! This module defines the top-level state hierarchy for the TUI:
//! - `AppState` - combined state (TuiState + overlay)
//! - `TuiState` - non-overlay UI state (input, transcript, session, auth, agent)
//! - `AgentState` - agent execution state (idle, waiting, streaming)
//!
//! ## State Hierarchy
//!
//! ```text
//! AppState
//! ├── tui: TuiState
//! │   ├── input: InputState      (user input, command history)
//! │   ├── transcript: TranscriptState (cells, scroll, layout)
//! │   ├── conversation: SessionState (messages, usage)
//! │   ├── session_ops: SessionOpsState (async operations)
//! │   ├── auth: AuthState        (authentication status)
//! │   └── agent_state: AgentState (execution state)
//! └── overlay: Option<Overlay>   (modal overlays)
//! ```
//!
//! ## Split State Architecture
//!
//! State is split between `TuiState` (non-overlay) and `Option<Overlay>`:
//! - `TuiState` contains all non-overlay UI state
//! - `Option<Overlay>` holds the active overlay if any
//! - `AppState` combines both for runtime use
//!
//! This allows overlay handlers to get `&mut self` and `&mut TuiState` simultaneously.

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::config::Config;
use crate::core::agent::AgentOptions;
use crate::core::session::Session;
use crate::providers::anthropic::ChatMessage;

// Feature state imports
use crate::modes::tui::auth::AuthState;
use crate::modes::tui::input::InputState;
use crate::modes::tui::overlays::Overlay;
use crate::modes::tui::session::{SessionOpsState, SessionState};
use crate::modes::tui::transcript::{HistoryCell, TranscriptState};

// ============================================================================
// AppState (Combined State)
// ============================================================================

/// Combined application state for the TUI.
///
/// Combines `TuiState` with `Option<Overlay>` to enable the split state
/// architecture where overlay handlers can access both without borrow conflicts.
pub struct AppState {
    pub tui: TuiState,
    pub overlay: Option<Overlay>,
}

impl AppState {
    /// Creates a new AppState.
    #[cfg(test)]
    pub fn new(
        config: Config,
        root: PathBuf,
        system_prompt: Option<String>,
        session: Option<Session>,
    ) -> Self {
        Self::with_history(config, root, system_prompt, session, Vec::new())
    }

    /// Creates an AppState with pre-loaded message history.
    ///
    /// Used for resuming previous sessions.
    pub fn with_history(
        config: Config,
        root: PathBuf,
        system_prompt: Option<String>,
        session: Option<Session>,
        history: Vec<ChatMessage>,
    ) -> Self {
        Self {
            tui: TuiState::with_history(config, root, system_prompt, session, history),
            overlay: None,
        }
    }
}

// ============================================================================
// AgentState
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
        cell_id: crate::modes::tui::transcript::CellId,
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

/// TUI application state (non-overlay).
///
/// This contains all state except for overlays. Overlays are stored separately
/// in `Option<Overlay>` and combined via `AppState` to enable the split state
/// architecture where overlay handlers can access both without borrow conflicts.
pub struct TuiState {
    /// Flag indicating the app should quit.
    pub should_quit: bool,
    /// User input state (textarea, history, navigation).
    pub input: InputState,
    /// Transcript display state (cells, scroll, layout, cache).
    pub transcript: TranscriptState,
    /// Session and conversation state (session, messages, usage).
    pub conversation: SessionState,
    /// Session async operations state (loading, creating, previewing).
    pub session_ops: SessionOpsState,
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
            session_ops: SessionOpsState::new(),
            auth,
            config,
            agent_opts,
            system_prompt,
            agent_state: AgentState::Idle,
            spinner_frame: 0,
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

    /// Resets conversation state for a new session.
    ///
    /// Clears transcript, messages, usage, and input history.
    /// Does NOT clear the session handle - a new session should be
    /// created via effect after calling this.
    pub fn reset_conversation(&mut self) {
        self.transcript.reset();
        self.conversation.reset();
        self.input.clear_history();
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
