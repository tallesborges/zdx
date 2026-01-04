//! Core dispatcher and event aggregation.
//!
//! Contains the `UiEvent` aggregator. The main `update()` function lives in
//! `reducer.rs` and `view()` in `view.rs`.
//!
//! See `docs/ARCHITECTURE.md` for the full TUI architecture overview.

pub mod events;

// Re-export event types for convenience (used by runtime, reducer)
#[allow(unused_imports)]
pub use events::{SessionUiEvent, UiEvent};
