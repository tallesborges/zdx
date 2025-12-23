//! UI event types.
//!
//! This module defines the unified event enum for the TUI.
//! All external inputs (terminal, engine, async results) are converted to `UiEvent`
//! before being processed by the reducer.

use crossterm::event::Event as CrosstermEvent;

use crate::core::events::EngineEvent;

/// Unified event enum for the TUI.
///
/// All inputs to the TUI are converted to this type before processing.
/// The reducer (`update`) pattern-matches on these events to update state.
#[derive(Debug)]
pub enum UiEvent {
    /// Timer tick (for animation, polling).
    Tick,

    /// Terminal input event (key, mouse, paste, resize).
    Terminal(CrosstermEvent),

    /// Engine event (streaming deltas, tool events, completion, etc.).
    Engine(EngineEvent),

    /// Async login token exchange completed.
    LoginResult(Result<(), String>),
}
