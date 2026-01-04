//! Input feature slice.
//!
//! Owns input state, keyboard handling, and handoff logic.
//!
//! ## Module Structure
//!
//! - `state.rs`: InputState, HandoffState - all input-related state
//! - `reducer.rs`: Key handling, input submission, handoff result handling
//! - `view.rs`: Input area rendering (normal and handoff modes)
//!
//! See `docs/ARCHITECTURE.md` for the TUI architecture overview.

mod reducer;
mod state;
mod view;

// Re-export state types
// Re-export reducer functions
pub use reducer::{handle_handoff_result, handle_main_key, handle_paste};
pub use state::{HandoffState, InputState};
// Re-export view functions
pub use view::{calculate_input_height, render_input};
