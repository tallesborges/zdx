//! Thread feature slice.
//!
//! Owns thread state, thread events, and thread-related update logic.
//!
//! ## Module Structure
//!
//! - `state.rs`: ThreadState, ThreadOpsState, ThreadUsage - in-memory thread state
//! - `update.rs`: Thread event handlers (loading, switching, creating, renaming)
//! - `render.rs`: Thread picker overlay rendering
//!
//! See `docs/ARCHITECTURE.md` for the TUI architecture overview.

mod render;
mod state;
mod update;

// Re-export state types
// Re-export view functions
pub use render::render_thread_picker;
pub use state::{ThreadOpsState, ThreadState, ThreadUsage};
// Re-export reducer functions
pub use update::{ThreadOverlayAction, handle_thread_event};
