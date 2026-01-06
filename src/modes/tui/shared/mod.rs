//! Shared leaf types for TUI features.
//!
//! Contains types with no feature dependencies (effects, commands, clipboard).
//! These types are shared across all feature modules.
//!
//! IMPORTANT: This module must NOT import UiEvent or feature-specific state
//! to avoid circular dependencies.

pub mod clipboard;
pub mod commands;
pub mod effects;
pub mod internal;
pub mod text;

pub use clipboard::Clipboard;
pub use text::sanitize_for_display;
#[allow(unused_imports)]
pub use internal::{
    AuthCommand, ConfigCommand, InputCommand, SessionCommand, StateCommand, TranscriptCommand,
};
