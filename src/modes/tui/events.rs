//! Re-export module for backward compatibility during migration.
//!
//! The canonical location is `crate::modes::tui::core::events`.
//! This re-export will be removed once all imports are updated.

pub use crate::modes::tui::core::events::{SessionUiEvent, UiEvent};
