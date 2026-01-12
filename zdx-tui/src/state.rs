//! Application state composition.
//!
//! This module defines the top-level state hierarchy for the TUI:
//! - `AppState` - combined state (TuiState + overlay)
//! - `TuiState` - non-overlay UI state (input, transcript, thread, auth, agent)
//! - `AgentState` - agent execution state (idle, waiting, streaming)
//!
//! ## State Hierarchy
//!
//! ```text
//! AppState
//! ├── tui: TuiState
//! │   ├── input: InputState      (user input, command history)
//! │   ├── transcript: TranscriptState (cells, scroll, layout)
//! │   ├── thread: ThreadState (messages, usage)
//! │   ├── task_seq: TaskSeq (async task id generator)
//! │   ├── tasks: Tasks (task lifecycle state)
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
use std::sync::Arc;

use tokio::sync::mpsc;
use zdx_core::config::Config;
use zdx_core::core::agent::AgentOptions;
use zdx_core::core::events::AgentEvent;
use zdx_core::core::thread_log::ThreadLog;
use zdx_core::providers::{ChatContentBlock, ChatMessage};

// Feature state imports
use crate::auth::AuthState;
use crate::common::{TaskSeq, Tasks};
use crate::input::InputState;
use crate::overlays::Overlay;
use crate::thread::ThreadState;
use crate::transcript::{CellId, HistoryCell, TranscriptState};

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
        thread_log: Option<ThreadLog>,
    ) -> Self {
        Self::with_history(config, root, system_prompt, thread_log, Vec::new())
    }

    /// Creates an AppState with pre-loaded message history.
    ///
    /// Used for resuming previous threads.
    pub fn with_history(
        config: Config,
        root: PathBuf,
        system_prompt: Option<String>,
        thread_log: Option<ThreadLog>,
        history: Vec<ChatMessage>,
    ) -> Self {
        Self {
            tui: TuiState::with_history(config, root, system_prompt, thread_log, history),
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
/// The task sends events through the channel, including `TurnCompleted` when done.
#[derive(Debug)]
pub enum AgentState {
    /// No agent task running, ready for input.
    Idle,
    /// Streaming response in progress.
    Streaming {
        /// Receiver for agent events.
        rx: mpsc::Receiver<Arc<AgentEvent>>,
        /// ID of the streaming assistant cell in transcript.
        cell_id: CellId,
        /// Buffered delta text to apply on next tick (coalescing).
        pending_delta: String,
    },
    /// Waiting for first response.
    Waiting {
        /// Receiver for agent events.
        rx: mpsc::Receiver<Arc<AgentEvent>>,
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
    /// Thread and thread state (thread log, messages, usage).
    pub thread: ThreadState,
    /// Task id sequence for async operations.
    pub task_seq: TaskSeq,
    /// Task lifecycle state for async operations.
    pub tasks: Tasks,
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
    /// Currently running bash command info (id, command) - results come via inbox.
    pub bash_running: Option<(String, String)>,
    /// Git branch name (cached at startup).
    pub git_branch: Option<String>,
    /// Shortened display path (cached at startup).
    pub display_path: String,
}

impl TuiState {
    /// Creates a TuiState with pre-loaded message history.
    ///
    /// Used for resuming previous threads.
    pub fn with_history(
        config: Config,
        root: PathBuf,
        system_prompt: Option<String>,
        thread_log: Option<ThreadLog>,
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
        let transcript = TranscriptState::with_cells(transcript_cells);

        // Create input state with command history
        let mut input = InputState::new();
        input.history = command_history;

        // Create thread state with history
        let thread = ThreadState::with_thread(thread_log, history);

        // Create auth state
        let auth = AuthState::new();

        Self {
            should_quit: false,
            input,
            transcript,
            thread,
            task_seq: TaskSeq::default(),
            tasks: Tasks::default(),
            auth,
            config,
            agent_opts,
            system_prompt,
            agent_state: AgentState::Idle,
            spinner_frame: 0,
            bash_running: None,
            git_branch,
            display_path,
        }
    }

    /// Builds transcript cells from message history.
    fn build_transcript_from_history(messages: &[ChatMessage]) -> Vec<HistoryCell> {
        use zdx_core::providers::MessageContent;

        let mut transcript = Vec::new();

        for msg in messages {
            match &msg.content {
                MessageContent::Text(t) => {
                    if t.is_empty() {
                        continue;
                    }
                    let cell = match msg.role.as_str() {
                        "user" => HistoryCell::user(t),
                        "assistant" => HistoryCell::assistant(t),
                        _ => continue,
                    };
                    transcript.push(cell);
                }
                MessageContent::Blocks(blocks) => match msg.role.as_str() {
                    "assistant" => {
                        let mut text_buffer = String::new();
                        let flush_text = |out: &mut Vec<HistoryCell>, buf: &mut String| {
                            if !buf.is_empty() {
                                out.push(HistoryCell::assistant(buf.clone()));
                                buf.clear();
                            }
                        };

                        for block in blocks {
                            match block {
                                ChatContentBlock::Reasoning(reasoning) => {
                                    flush_text(&mut transcript, &mut text_buffer);
                                    if let Some(text) = &reasoning.text
                                        && !text.is_empty()
                                    {
                                        let mut cell = HistoryCell::thinking_streaming(text);
                                        cell.finalize_thinking(reasoning.replay.clone());
                                        transcript.push(cell);
                                    }
                                }
                                ChatContentBlock::Text(text) => {
                                    if !text_buffer.is_empty() {
                                        text_buffer.push('\n');
                                    }
                                    text_buffer.push_str(text);
                                }
                                _ => {}
                            }
                        }

                        flush_text(&mut transcript, &mut text_buffer);
                    }
                    "user" => {
                        // Extract text blocks, ignore tool use/result for display
                        let text = blocks
                            .iter()
                            .filter_map(|b| match b {
                                ChatContentBlock::Text(t) => Some(t.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !text.is_empty() {
                            transcript.push(HistoryCell::user(&text));
                        }
                    }
                    _ => {}
                },
            }
        }

        transcript
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
