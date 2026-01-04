//! Re-export hub for event types.
//!
//! The canonical location is `crate::modes::tui::core::events`.
//! This module re-exports types for convenience at the `tui` level.

#[allow(unused_imports)]
pub use crate::modes::tui::core::events::{SessionUiEvent, UiEvent};
