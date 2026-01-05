//! Session feature slice.
//!
//! Owns session state, session events, and session-related update logic.
//!
//! ## Module Structure
//!
//! - `state.rs`: SessionState, SessionOpsState, SessionUsage - session and conversation state
//! - `update.rs`: Session event handlers (loading, switching, creating, renaming)
//! - `render.rs`: Session picker overlay rendering
//!
//! See `docs/ARCHITECTURE.md` for the TUI architecture overview.

mod render;
mod state;
mod update;

// Re-export state types
// Re-export view functions
pub use render::render_session_picker;
pub use state::{SessionOpsState, SessionState, SessionUsage};
// Re-export reducer functions
pub use update::{SessionOverlayAction, handle_session_event};
