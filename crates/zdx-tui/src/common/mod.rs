//! Shared leaf types for TUI features.
//!
//! Contains types with no feature dependencies (clipboard, tasks, text helpers).
//! These types are shared across all feature modules.
//!
//! IMPORTANT: This module must NOT import `UiEvent` or feature-specific state
//! to avoid circular dependencies.

pub mod clipboard;
pub mod commands;
pub mod notify;
pub mod scrollbar;
pub mod task;

pub use clipboard::Clipboard;
pub use scrollbar::Scrollbar;
pub use task::{TaskCompleted, TaskId, TaskKind, TaskMeta, TaskSeq, TaskStarted, Tasks};
// Text helpers now live in the shared `zdx-transcript` crate; re-export them
// here so existing `crate::common::…` call sites keep working.
pub use zdx_transcript::text::{
    ratatui_text, ratatui_width, sanitize_for_display, truncate_start_with_ellipsis,
    truncate_with_ellipsis,
};
