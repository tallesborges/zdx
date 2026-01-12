//! Input feature slice.
//!
//! Owns input state, keyboard handling, and handoff logic.
//!
//! ## Module Structure
//!
//! - `state.rs`: InputState, HandoffState - all input-related state
//! - `update.rs`: Key handling, input submission, handoff result handling
//! - `render.rs`: Input area rendering (normal and handoff modes)
//!
//! See `docs/ARCHITECTURE.md` for the TUI architecture overview.

mod render;
mod state;
mod update;

// Re-export state types
// Re-export reducer functions
// Re-export view functions
pub use render::{calculate_input_height, render_input};
pub use state::{HandoffState, InputState};
pub use update::{build_send_effects, handle_handoff_result, handle_main_key, handle_paste};
