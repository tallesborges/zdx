//! UI event types.
//!
//! This module defines the unified event enum for the TUI.
//! All external inputs (terminal, agent, async results) are converted to `UiEvent`
//! before being processed by the reducer.

use crossterm::event::Event as CrosstermEvent;

use crate::core::events::AgentEvent;

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

    /// Async login token exchange completed.
    LoginResult(Result<(), String>),

    /// Async handoff generation completed (Ok = generated prompt, Err = error message).
    HandoffResult(Result<String, String>),

    /// File discovery completed for file picker.
    FilesDiscovered(Vec<std::path::PathBuf>),
}
