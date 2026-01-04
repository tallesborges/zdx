//! Auth feature slice.
//!
//! Owns authentication state, login flow handling, and login overlay rendering.
//!
//! ## Module Structure
//!
//! - `state.rs`: AuthStatus, AuthState - authentication type and login flow state
//! - `reducer.rs`: Login result handling, OAuth flow state transitions
//! - `view.rs`: Login overlay rendering
//!
//! See `docs/ARCHITECTURE.md` for the TUI architecture overview.

mod reducer;
mod state;
mod view;

// Re-export state types
pub use state::{AuthState, AuthStatus};
// Re-export reducer functions
pub use reducer::handle_login_result;
// Re-export view functions
pub use view::render_login_overlay;
