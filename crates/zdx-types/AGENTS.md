# zdx-types development guide

Scope: pure shared value types, enums, and associated pure helper logic used across providers, tools, agent events, and thread persistence. No I/O, HTTP, config loading, or runtime services.

## Where things are

- `src/lib.rs`: crate root with top-level re-exports
- `src/config.rs`: `ThinkingLevel`, `TextVerbosity` — config-related value enums with pure helper methods
- `src/messages.rs`: `ChatMessage`, `ChatContentBlock`, `MessageContent`, `ReasoningBlock`, `ReplayToken`, `ContentBlockType`, `SignatureProvider` — message construction helpers
- `src/providers.rs`: `ProviderErrorKind`, `ProviderError`, `ProviderResult`, `Usage`, `StreamEvent`, `ProviderStream` — provider error/retry classification and streaming types
- `src/events.rs`: `AgentEvent`, `ErrorKind`, `TurnStatus`, `ToolOutput`, `ToolError`, `ImageContent` — agent event and tool output types with serialization logic
- `src/tools.rs`: `ToolDefinition`, `ToolResult`, `ToolResultBlock`, `ToolResultContent`, `tool_command_text` (one-line primary command/target summary for a tool call; shared by `zdx-transcript` display and the engine's active-run marker)
- `src/tool_view.rs`: `tool_view(name, input, output, duration_ms, limits)` (raw envelope) and `tool_output_view` (same arguments, typed `ToolOutput`, no copy) → `ToolView`, the display projection of one tool call (outcome incl. `timed_out`, head with a leading `cd X &&` split off only when nothing after it can run in another directory, body by kind: streams (any result with stdout/stderr, not only bash), code, matches with context lines, paths, line diff (edits, and writes that created the file; an overwrite shows its content), markdown, error, text; status facts tagged `outcome`/`duration`/`detail` so a host can leave out what it already shows, row hint, warnings in `notes`, plain long details such as background log paths in `info`). A background job (`background: true` start or `backgrounded` handoff) reports its id, pid, and logs, never a byte size. How much output it carries is the caller's `ViewLimits`: `DISPLAY` (64 KB per text, 2000 rows incl. grep context lines, a note when cut) for a surface that keeps the raw output elsewhere (the Mini App), `FULL` for one whose view is the only way to read it (the terminal). Rendered by the Mini App (`zdx-engine`'s `orchestration/view_bodies/threads.rs` attaches it to transcript tool items) and by the terminal (`zdx_transcript::tool_detail_body`, used by the TUI popup and the monitor pane); add a tool's case here, not in a surface
- `src/images.rs`: `sniff_raster_image_mime` — PNG/JPEG/GIF/WebP detection from magic bytes; shared by the `Read` tool and the OpenAI Images response parser
- `src/logging.rs`: `log_field(value, max_bytes)` — collapses whitespace and truncates a value for use as a `tracing` field, so one event stays one log line and stays bounded. Used by providers (HTTP error bodies), engine tools (tool errors), agent turn errors, and subagent stderr.

## Conventions

- This crate must not depend on any other local crate.
- No I/O, HTTP, config loading, or runtime services.
- Types may include pure helper methods (constructors, serialization, classification) as long as they have no side effects.
- Keep it small: only move types here when they are genuinely shared across 2+ sibling crates.

## Checks

- Follow the root verification rules. Scoped tests: `cargo test -p zdx-types --lib --bins --tests`.
