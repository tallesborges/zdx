//! Auth feature slice.
//!
//! Owns authentication state, login flow handling, and login overlay rendering.
//!
//! ## Module Structure
//!
//! - `state.rs`: AuthStatus, AuthState - authentication type and login flow state
//! - `update.rs`: Login result handling, OAuth flow state transitions
//! - `render.rs`: Login overlay rendering
//!
//! See `docs/ARCHITECTURE.md` for the TUI architecture overview.

mod render;
mod state;
mod update;

// Re-export state types
// Re-export view functions
pub use render::render_login_overlay;
pub use state::{AuthState, AuthStatus};
// Re-export reducer functions
pub use update::{LoginOverlayAction, handle_login_result};
