//! Session feature slice.
//!
//! Owns session state, session events, and session-related update logic.
//!
//! ## Module Structure
//!
//! - `state.rs`: SessionState, SessionOpsState, SessionUsage - session and conversation state
//! - `reducer.rs`: Session event handlers (loading, switching, creating, renaming)
//! - `view.rs`: Session picker overlay rendering
//!
//! See `docs/ARCHITECTURE.md` for the TUI architecture overview.

mod reducer;
mod state;
mod view;

// Re-export state types
pub use state::{SessionOpsState, SessionState, SessionUsage};
// Re-export reducer functions
pub use reducer::handle_session_event;
// Re-export view functions
pub use view::render_session_picker;
