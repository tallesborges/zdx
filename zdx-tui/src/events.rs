//! UI event types.
//!
//! This module defines the unified event enum for the TUI.
//! All external inputs (terminal, agent, async results) are converted to `UiEvent`
//! before being processed by the reducer.
//!
//! ## Inbox Pattern
//!
//! Events follow the "inbox" pattern where async operations send events directly
//! to the runtime's event inbox. Results arrive as separate events.
//!
//! ## Task Lifecycle Events
//!
//! Async work uses a uniform lifecycle:
//! - The runtime emits `UiEvent::TaskStarted` once a task is actually spawned
//! - The runtime emits `UiEvent::TaskCompleted` with the result event when done
//! - The reducer is the only place that mutates `TaskState`
//!
//! ## Cancellation Convention
//!
//! Cancelable operations use `tokio_util::sync::CancellationToken` uniformly:
//! - `TaskStarted` carries the token for the reducer to store
//! - The runtime spawns tasks that `select!` on `token.cancelled()` vs work
//! - Cancellation is initiated via `UiEffect::CancelTask` which calls `token.cancel()`
//! - This keeps the runtime as a "dumb executor" and reducer as the source of truth

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::Event as CrosstermEvent;
use tokio::sync::mpsc;
use zdx_core::core::events::{AgentEvent, ToolOutput};
use zdx_core::core::thread_log::{ThreadLog, ThreadSummary, Usage};
use zdx_core::providers::ChatMessage;

use crate::common::{RequestId, TaskCompleted, TaskKind, TaskStarted};
use crate::transcript::HistoryCell;

/// Thread event enum for async thread operations.
///
/// Results-only events for thread I/O. Loading flags are set by the reducer
/// via mutations when emitting effects, not via separate `*Started` events.
#[derive(Debug)]
pub enum ThreadUiEvent {
    /// Thread list loaded for picker.
    ListLoaded {
        threads: Vec<ThreadSummary>,
        original_cells: Vec<HistoryCell>,
    },

    /// Thread list load failed.
    ListFailed { error: String },

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

    /// Thread preview loaded (for thread picker navigation).
    PreviewLoaded {
        req: RequestId,
        cells: Vec<HistoryCell>,
    },

    /// Thread preview load failed (silent - just don't update).
    PreviewFailed { req: RequestId },

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

    /// Thread rename succeeded.
    Renamed {
        thread_id: String,
        title: Option<String>,
    },

    /// Thread rename failed.
    RenameFailed { error: String },

    /// Auto thread title suggestion completed (None if skipped/failed).
    TitleSuggested {
        thread_id: String,
        title: Option<String>,
    },
}

/// Unified event enum for the TUI.
///
/// All inputs to the TUI are converted to this type before processing.
/// The reducer (`update`) pattern-matches on these events to update state.
///
/// ## Inbox Pattern
///
/// With the inbox pattern, async operations send events directly to the runtime's
/// event inbox. `TaskStarted`/`TaskCompleted` provide a uniform lifecycle for
/// task state and latest-only gating.
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
    LoginResult {
        req: RequestId,
        result: Result<(), String>,
    },

    /// Local OAuth callback returned with an optional code.
    LoginCallbackResult(Option<String>),

    /// Async handoff generation completed (Ok = generated prompt, Err = error message).
    HandoffResult(Result<String, String>),

    /// Handoff thread creation succeeded.
    HandoffThreadCreated {
        thread_log: ThreadLog,
        context_paths: Vec<PathBuf>,
        prompt: String,
    },

    /// Handoff thread creation failed.
    HandoffThreadCreateFailed { error: String },

    /// File discovery completed.
    FilesDiscovered(Vec<PathBuf>),

    /// Clipboard copy completed successfully.
    ClipboardCopied,

    /// Direct bash execution completed.
    BashExecuted { id: String, result: ToolOutput },

    /// Task lifecycle: runtime started a task (cancel token optional).
    TaskStarted {
        kind: TaskKind,
        started: TaskStarted,
    },

    /// Task lifecycle: runtime completed a task (wraps the result event).
    TaskCompleted {
        kind: TaskKind,
        completed: TaskCompleted<Box<UiEvent>>,
    },

    /// Thread async I/O results.
    Thread(ThreadUiEvent),
}
