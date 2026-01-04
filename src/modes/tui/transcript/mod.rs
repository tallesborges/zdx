//! Transcript model for TUI rendering.
//!
//! This module defines the transcript types that form the source of truth
//! for the TUI. The transcript is width-agnostic; wrapping happens at
//! display time for the current terminal width.
//!
//! See SPEC.md §9 for the contract.

mod cell;
mod style;
mod wrap;

pub use cell::{CellId, HistoryCell, ToolState};
pub use style::{Style, StyledLine, StyledSpan};
pub use wrap::WrapCache;
