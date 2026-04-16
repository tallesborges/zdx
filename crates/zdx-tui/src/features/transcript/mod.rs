//! Transcript model for TUI rendering.
//!
//! This module defines the transcript types that form the source of truth
//! for the TUI. The transcript is width-agnostic; wrapping happens at
//! display time for the current terminal width.
//!
//! See SPEC.md §9 for the contract.

// Existing modules (display types)
mod cell;
pub mod markdown;
mod style;
mod wrap;

// New feature slice modules
mod build;
mod reasoning;
mod render;
mod selection;
mod state;
mod update;

// Re-export existing display types
// Re-export build function
pub use build::build_transcript_from_events;
pub use cell::{CellId, HistoryCell, ToolState};
// Re-export the shared reasoning-display helper so the message-reload path
// in `crate::state` can share the same display logic as build.rs and
// update.rs.
pub(crate) use reasoning::reasoning_display_text;
// Re-export render functions
pub use render::{SPINNER_SPEED_DIVISOR, calculate_cell_line_counts, render_transcript};
// Re-export selection types (only those used externally)
pub use selection::{LineInteraction, LineMapping, SelectionState};
// Re-export scroll types
pub use state::{ScrollMode, ScrollState};
// Re-export state types
pub use state::{TranscriptState, VisibleRange};
pub use style::{Style, StyledLine, StyledSpan};
// Re-export update functions
pub use update::{apply_pending_delta, apply_scroll_delta, handle_agent_event, handle_mouse};
pub use wrap::WrapCache;
