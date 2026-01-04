//! Re-export transcript state types from transcript feature.
//!
//! This shim maintains backward compatibility during the feature-slice migration.
//! The actual implementation lives in `crate::modes::tui::transcript::state`.

pub use crate::modes::tui::transcript::{TranscriptState, VisibleRange};

// Re-export test-only types
#[cfg(test)]
pub use crate::modes::tui::transcript::{ScrollMode, ScrollState};
