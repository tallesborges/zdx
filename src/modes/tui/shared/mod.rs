//! Shared leaf types for TUI features.
//!
//! Contains types with no feature dependencies (effects, mutations, clipboard).
//! These types are shared across all feature modules.
//!
//! IMPORTANT: This module must NOT import UiEvent or feature-specific state
//! to avoid circular dependencies.

pub mod clipboard;
pub mod commands;
pub mod effects;
pub mod internal;
pub mod scrollbar;
pub mod text;

pub use clipboard::Clipboard;
#[allow(unused_imports)]
pub use internal::{
    AuthMutation, ConfigMutation, InputMutation, StateMutation, ThreadMutation, TranscriptMutation,
};
pub use scrollbar::Scrollbar;
pub use text::sanitize_for_display;
