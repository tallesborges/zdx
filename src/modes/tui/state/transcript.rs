//! Re-export transcript state types from transcript feature.
//!
//! The actual implementation lives in `crate::modes::tui::transcript::state`.
//! This module re-exports for convenience at the `state` level.

pub use crate::modes::tui::transcript::TranscriptState;

// Re-export test-only types
#[cfg(test)]
pub use crate::modes::tui::transcript::{ScrollMode, ScrollState};
