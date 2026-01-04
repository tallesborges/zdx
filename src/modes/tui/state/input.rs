//! User input state (re-exports from input feature slice).
//!
//! This module re-exports types from `crate::modes::tui::input::state`
//! for backward compatibility. New code should import directly from
//! `crate::modes::tui::input`.

pub use crate::modes::tui::input::{HandoffState, InputState};
