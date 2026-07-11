# zdx-tui development guide

Scope: full-screen interactive TUI (state/update/render/effects/runtime).

## Where things are

- `src/lib.rs`: TUI exports (`run_interactive_chat`, `TuiRuntime`)
- `src/terminal.rs`: terminal setup/restore + panic hooks
- `src/state.rs`: `AppState` + TUI state structs
- `src/events.rs`: UI event types
- `src/update.rs`: reducer/update orchestration
- `src/render.rs`: render orchestration
- `src/effects.rs`: effect descriptions
- `src/mutations.rs`: state mutation helpers

### Runtime (`src/runtime/`)

- `runtime/mod.rs`: runtime event loop and dispatcher
- `runtime/inbox.rs`: runtime inbox channel types
- `runtime/image_ops.rs`: shared image loading/transform helpers (preview + attachments)
- `runtime/handlers/voice.rs`: microphone capture + voice transcription task handlers
- `runtime/handlers/`: side-effect handlers (thread ops, agent spawn, auth, skills)
- `runtime/handoff.rs`: handoff generation handlers (thin adapter over `zdx_engine::core::handoff_generation`)
- `runtime/prompt_builder.rs`: prompt-builder generation handlers (thin adapter over `zdx_engine::core::prompt_builder_generation`)
- `runtime/thread_title.rs`: auto-title handlers
- `runtime/thread_tldr.rs`: thread TLDR/recap handlers

### Feature slices (`src/features/`)

- `features/auth/`: auth feature slice
- `features/input/`: input feature slice (`text_buffer.rs` cursor editing)
- `features/statusline/`: debug status line state/render
- `features/thread/`: thread picker + thread tree view
- `features/transcript/`: transcript feature + markdown rendering (`reasoning.rs` shared reasoning-display helper + `[redacted reasoning]` placeholder constant)

### Other modules

- `src/common/`: shared leaf types
- `src/overlays/`: command palette, skill picker, rename overlays
- `src/overlays/tldr.rs`: thread TLDR/recap overlay (Ctrl+R)
- `src/overlays/tool_detail.rs`: tool detail popup overlay (full args/output/status on click)
- `src/overlays/followup_picker.rs`: end-of-turn follow-up suggestion picker (Ctrl+F; sends selection as next message)

## Conventions

- Keep Elm/MVU boundaries clear: state/update/render/effects separated.
- Prefer pure render/update functions; isolate side effects in runtime handlers.

## Checks

- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration for this crate: `cargo nextest run -p zdx-tui`
- Use `just lint` or `just test` only when intentionally running one half of CI

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.
- Add/move/delete prompt layer files in this crate: update this file.
- Architecture changes: update `docs/ARCHITECTURE.md`..