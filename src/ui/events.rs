//! UI event types.
//!
//! This module defines the unified event enum for the TUI.
//! All external inputs (terminal, engine, async results) are converted to `UiEvent`
//! before being processed by the reducer.

use crossterm::event::Event as CrosstermEvent;

use crate::core::events::EngineEvent;
use crate::providers::anthropic::ChatMessage;

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

    /// Engine event (streaming deltas, tool events, etc.).
    Engine(EngineEvent),

    /// Engine turn completed with final result.
    TurnFinished(TurnResult),

    /// Async login token exchange completed.
    LoginResult(Result<(), String>),
}

/// Result of an engine turn.
#[derive(Debug)]
pub enum TurnResult {
    /// Turn completed successfully with final text and updated messages.
    Success {
        final_text: String,
        messages: Vec<ChatMessage>,
    },
    /// Turn failed with an error.
    Error(String),
    /// Turn was interrupted by user.
    Interrupted,
}
