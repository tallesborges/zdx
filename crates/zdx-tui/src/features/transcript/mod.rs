//! Transcript model for TUI rendering.
//!
//! This module defines the transcript types that form the source of truth
//! for the TUI. The transcript is width-agnostic; wrapping happens at
//! display time for the current terminal width.
//!
//! See SPEC.md §9 for the contract.

// New feature slice modules
mod render;
mod selection;
mod state;
mod update;

// Shared transcript display model + rendering now live in the `zdx-transcript`
// crate so non-interactive consumers (e.g. the monitor) can reuse them.
// Re-export render functions
pub use render::{SPINNER_SPEED_DIVISOR, calculate_cell_line_counts, render_transcript};
// Re-export selection types (only those used externally)
pub use selection::{LineInteraction, LineMapping, SelectionState};
// Re-export scroll types
pub use state::{ScrollMode, ScrollState};
// Re-export state types
pub use state::{TranscriptState, VisibleRange};
// Re-export update functions
pub use update::{apply_pending_delta, handle_agent_event, handle_mouse};
pub use zdx_transcript::{
    CellId, ChildToolEntry, ChildToolState, HistoryCell, Style, StyledLine, StyledSpan, ToolState,
    WrapCache, build_transcript_from_events, convert_styled_line, markdown, reasoning_display_text,
};
