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

mod update;
mod state;
mod render;

// Re-export state types
pub use state::{AuthState, AuthStatus};
// Re-export reducer functions
pub use update::{handle_login_result, LoginOverlayAction};
// Re-export view functions
pub use render::render_login_overlay;
