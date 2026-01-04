//! Core dispatcher and event aggregation.
//!
//! Contains the `UiEvent` aggregator and top-level `update()`/`render()` dispatch.
//! See `docs/plans/tui-feature-slice-migration.md` for migration plan.

pub mod events;

// Re-export event types for convenience (used by runtime, reducer)
#[allow(unused_imports)]
pub use events::{SessionUiEvent, UiEvent};

// TODO: create update dispatcher (Slice 4)
