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

mod update;
mod state;
mod render;

// Re-export state types
pub use state::{SessionOpsState, SessionState, SessionUsage};
// Re-export reducer functions
pub use update::{handle_session_event, SessionOverlayAction};
// Re-export view functions
pub use render::render_session_picker;
