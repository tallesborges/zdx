//! Authentication state (re-export shim).
//!
//! This module re-exports from the auth feature slice for backward compatibility.
//! See `src/modes/tui/auth/` for the actual implementation.

pub use crate::modes::tui::auth::{AuthState, AuthStatus};
