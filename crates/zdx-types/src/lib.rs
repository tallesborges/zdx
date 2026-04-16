//! Pure, shared ZDX value types (DTOs/enums) with no runtime dependencies.
//!
//! This crate contains plain data shapes used across providers, tools,
//! agent events, and thread persistence. It must not take local crate
//! dependencies and must not contain I/O, HTTP, config loading, or
//! runtime service wiring.

pub mod events;
pub mod messages;
pub mod providers;
pub mod tools;

pub use events::{
    AgentEvent, ErrorKind, ImageContent, NoticeKind, ToolError, ToolOutput, TurnStatus,
};
pub use messages::{
    ChatContentBlock, ChatMessage, ContentBlockType, MessageContent, ReasoningBlock, ReplayToken,
    SignatureProvider,
};
pub use providers::{
    ProviderError, ProviderErrorKind, ProviderResult, ProviderStream, StreamEvent, Usage,
};
pub use tools::{ToolDefinition, ToolResult, ToolResultBlock, ToolResultContent};
pub mod config;
pub use config::{TextVerbosity, ThinkingLevel};
