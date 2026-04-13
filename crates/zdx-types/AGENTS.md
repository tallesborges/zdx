# zdx-types development guide

Scope: pure shared value types, enums, and associated pure helper logic used across providers, tools, agent events, and thread persistence. No I/O, HTTP, config loading, or runtime services.

## Where things are

- `src/lib.rs`: crate root with top-level re-exports
- `src/config.rs`: `ThinkingLevel`, `TextVerbosity` — config-related value enums with pure helper methods
- `src/messages.rs`: `ChatMessage`, `ChatContentBlock`, `MessageContent`, `ReasoningBlock`, `ReplayToken`, `ContentBlockType`, `SignatureProvider` — message construction helpers
- `src/providers.rs`: `ProviderErrorKind`, `ProviderError`, `ProviderResult`, `Usage`, `StreamEvent`, `ProviderStream` — provider error/retry classification and streaming types
- `src/events.rs`: `AgentEvent`, `ErrorKind`, `TurnStatus`, `ToolOutput`, `ToolError`, `ImageContent` — agent event and tool output types with serialization logic
- `src/tools.rs`: `ToolDefinition`, `ToolResult`, `ToolResultBlock`, `ToolResultContent`

## Conventions

- This crate must not depend on any other local crate.
- No I/O, HTTP, config loading, or runtime services.
- Types may include pure helper methods (constructors, serialization, classification) as long as they have no side effects.
- Keep it small: only move types here when they are genuinely shared across 2+ sibling crates.

## Checks

- `cargo test -p zdx-types`
- Final: `just ci`
