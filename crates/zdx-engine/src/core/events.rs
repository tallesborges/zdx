//! Agent event types for streaming and TUI.
//!
//! Pure value types now live in `zdx_types`. This module re-exports them so
//! existing `crate::core::events::*` imports keep working.

pub use zdx_types::{AgentEvent, ErrorKind, ImageContent, ToolError, ToolOutput, TurnStatus};
