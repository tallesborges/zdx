//! UI event types.
//!
//! This module defines the unified event enum for the TUI.
//! All external inputs (terminal, agent, async results) are converted to `UiEvent`
//! before being processed by the reducer.

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::Event as CrosstermEvent;
use tokio::sync::{mpsc, oneshot};

use crate::core::events::AgentEvent;
use crate::core::session::{Session, SessionSummary};
use crate::modes::tui::transcript::HistoryCell;
use crate::providers::anthropic::ChatMessage;

/// Session event enum for async session operations.
#[derive(Debug)]
pub enum SessionUiEvent {
    /// Session list loaded for picker.
    ListLoaded {
        sessions: Vec<SessionSummary>,
        original_cells: Vec<HistoryCell>,
    },

    /// Session list load failed.
    ListFailed { error: String },

    /// Session loaded successfully (for switching to a session).
    Loaded {
        session_id: String,
        cells: Vec<HistoryCell>,
        messages: Vec<ChatMessage>,
        history: Vec<String>,
        session: Option<Session>,
    },

    /// Session load failed.
    LoadFailed { error: String },

    /// Session preview loaded (for session picker navigation).
    PreviewLoaded { cells: Vec<HistoryCell> },

    /// Session preview load failed (silent - just don't update).
    PreviewFailed,

    /// New session created successfully.
    Created {
        session: Session,
        context_paths: Vec<PathBuf>,
    },

    /// New session creation failed.
    CreateFailed { error: String },

    /// Session rename succeeded.
    Renamed {
        session_id: String,
        title: Option<String>,
    },

    /// Session rename failed.
    RenameFailed { error: String },
}

/// Unified event enum for the TUI.
///
/// All inputs to the TUI are converted to this type before processing.
/// The reducer (`update`) pattern-matches on these events to update state.
#[derive(Debug)]
pub enum UiEvent {
    /// Timer tick (for animation, polling).
    Tick,

    /// Frame event for per-frame state updates (layout, delta coalescing).
    ///
    /// Emitted once per frame before other events are processed.
    /// Contains terminal dimensions for layout calculations.
    Frame { width: u16, height: u16 },

    /// Terminal input event (key, mouse, paste, resize).
    Terminal(CrosstermEvent),

    /// Agent event (streaming deltas, tool events, completion, etc.).
    Agent(AgentEvent),

    /// Agent turn spawned; reducer should set agent state to Waiting.
    AgentSpawned {
        rx: mpsc::Receiver<Arc<AgentEvent>>,
    },

    /// Async login token exchange completed.
    LoginResult(Result<(), String>),

    /// Token exchange spawned; reducer should store login receiver.
    LoginExchangeStarted {
        rx: mpsc::Receiver<Result<(), String>>,
    },

    /// Async handoff generation completed (Ok = generated prompt, Err = error message).
    HandoffResult(Result<String, String>),

    /// Handoff generation spawned; reducer should set handoff generating state.
    HandoffGenerationStarted {
        goal: String,
        rx: mpsc::Receiver<Result<String, String>>,
        cancel_tx: oneshot::Sender<()>,
    },

    /// Handoff session creation succeeded.
    HandoffSessionCreated { session: Session },

    /// Handoff session creation failed.
    HandoffSessionCreateFailed { error: String },

    /// File discovery completed for file picker.
    FilesDiscovered(Vec<PathBuf>),

    /// Session async I/O results.
    Session(SessionUiEvent),
}
