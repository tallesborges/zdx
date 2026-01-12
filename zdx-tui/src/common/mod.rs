//! Shared leaf types for TUI features.
//!
//! Contains types with no feature dependencies (clipboard, tasks, text helpers).
//! These types are shared across all feature modules.
//!
//! IMPORTANT: This module must NOT import UiEvent or feature-specific state
//! to avoid circular dependencies.

pub mod clipboard;
pub mod commands;
pub mod request_id;
pub mod scrollbar;
pub mod task;
pub mod text;

pub use clipboard::Clipboard;
pub use request_id::{LatestOnly, RequestId};
pub use scrollbar::Scrollbar;
pub use task::{TaskCompleted, TaskId, TaskKind, TaskMeta, TaskSeq, TaskStarted, Tasks};
pub use text::sanitize_for_display;
