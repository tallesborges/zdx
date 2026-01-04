//! Transcript model for TUI rendering.
//!
//! This module defines the transcript types that form the source of truth
//! for the TUI. The transcript is width-agnostic; wrapping happens at
//! display time for the current terminal width.
//!
//! See SPEC.md §9 for the contract.

// Existing modules (display types)
mod cell;
mod style;
mod wrap;

// New feature slice modules
mod build;
mod selection;
mod state;
mod update;

// Re-export existing display types
pub use cell::{CellId, HistoryCell, ToolState};
pub use style::{Style, StyledLine, StyledSpan};
pub use wrap::WrapCache;

// Re-export state types
pub use state::{TranscriptState, VisibleRange};

// Re-export scroll types (used by tests in state/mod.rs via state/transcript.rs shim)
#[allow(unused_imports)] // Used in test configurations via state/transcript.rs shim
pub use state::{ScrollMode, ScrollState};

// Test-only exports (used in transcript/state.rs tests)
#[cfg(test)]
#[allow(unused_imports)]
pub use state::{CellLineInfo, ScrollAccumulator};

// Re-export selection types (only those used externally)
pub use selection::{LineMapping, SelectionState};

// Re-export build function
pub use build::build_transcript_from_events;

// Re-export update functions
pub use update::{apply_pending_delta, apply_scroll_delta, handle_agent_event, handle_mouse};
