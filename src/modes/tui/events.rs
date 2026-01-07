//! UI event types.
//!
//! This module defines the unified event enum for the TUI.
//! All external inputs (terminal, agent, async results) are converted to `UiEvent`
//! before being processed by the reducer.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crossterm::event::Event as CrosstermEvent;
use tokio::sync::{mpsc, oneshot};

use crate::core::events::AgentEvent;
use crate::core::thread_log::{ThreadLog, ThreadSummary, Usage};
use crate::modes::tui::transcript::HistoryCell;
use crate::providers::anthropic::ChatMessage;

/// Thread event enum for async thread operations.
#[derive(Debug)]
pub enum ThreadUiEvent {
    /// Thread list load started; reducer should store the receiver.
    ListStarted { rx: mpsc::Receiver<UiEvent> },

    /// Thread list loaded for picker.
    ListLoaded {
        threads: Vec<ThreadSummary>,
        original_cells: Vec<HistoryCell>,
    },

    /// Thread list load failed.
    ListFailed { error: String },

    /// Thread load started; reducer should store the receiver.
    LoadStarted { rx: mpsc::Receiver<UiEvent> },

    /// Thread loaded successfully (for switching to a thread).
    Loaded {
        thread_id: String,
        cells: Vec<HistoryCell>,
        messages: Vec<ChatMessage>,
        history: Vec<String>,
        thread_log: Option<ThreadLog>,
        /// Restored token usage: (cumulative, latest)
        usage: (Usage, Usage),
    },

    /// Thread load failed.
    LoadFailed { error: String },

    /// Thread preview load started; reducer should store the receiver.
    PreviewStarted { rx: mpsc::Receiver<UiEvent> },

    /// Thread preview loaded (for thread picker navigation).
    PreviewLoaded { cells: Vec<HistoryCell> },

    /// Thread preview load failed (silent - just don't update).
    PreviewFailed,

    /// Thread creation started; reducer should store the receiver.
    CreateStarted { rx: mpsc::Receiver<UiEvent> },

    /// Thread fork started; reducer should store the receiver.
    ForkStarted { rx: mpsc::Receiver<UiEvent> },

    /// New thread created successfully.
    Created {
        thread_log: ThreadLog,
        context_paths: Vec<PathBuf>,
    },

    /// Forked thread created successfully.
    ForkedLoaded {
        thread_id: String,
        cells: Vec<HistoryCell>,
        messages: Vec<ChatMessage>,
        history: Vec<String>,
        thread_log: ThreadLog,
        /// Restored token usage: (cumulative, latest)
        usage: (Usage, Usage),
        user_input: Option<String>,
        turn_number: usize,
    },

    /// New thread creation failed.
    CreateFailed { error: String },

    /// Thread fork failed.
    ForkFailed { error: String },

    /// Thread rename started; reducer should store the receiver.
    RenameStarted { rx: mpsc::Receiver<UiEvent> },

    /// Thread rename succeeded.
    Renamed {
        thread_id: String,
        title: Option<String>,
    },

    /// Thread rename failed.
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
    AgentSpawned { rx: mpsc::Receiver<Arc<AgentEvent>> },

    /// Async login token exchange completed.
    LoginResult(Result<(), String>),

    /// Token exchange spawned; reducer should store login receiver.
    LoginExchangeStarted {
        rx: mpsc::Receiver<Result<(), String>>,
    },

    /// Local OAuth callback listener spawned.
    LoginCallbackStarted { rx: mpsc::Receiver<Option<String>> },

    /// Local OAuth callback returned with an optional code.
    LoginCallbackResult(Option<String>),

    /// Async handoff generation completed (Ok = generated prompt, Err = error message).
    HandoffResult(Result<String, String>),

    /// Handoff generation spawned; reducer should set handoff generating state.
    /// Handoff generation spawned; reducer should set handoff generating state.
    HandoffGenerationStarted {
        goal: String,
        rx: oneshot::Receiver<Result<String, String>>,
        cancel: oneshot::Sender<()>,
    },

    /// Handoff thread creation succeeded.
    HandoffThreadCreated { thread_log: ThreadLog },

    /// Handoff thread creation failed.
    HandoffThreadCreateFailed { error: String },

    /// File discovery started.
    FileDiscoveryStarted {
        rx: oneshot::Receiver<Vec<PathBuf>>,
        cancel: Arc<AtomicBool>,
    },

    /// File discovery completed.
    FilesDiscovered(Vec<PathBuf>),

    /// Clipboard copy completed successfully.
    ClipboardCopied,

    /// Thread async I/O results.
    Thread(ThreadUiEvent),
}
