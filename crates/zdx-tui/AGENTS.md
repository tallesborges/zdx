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
- `runtime/handlers/`: side-effect handlers (thread ops, agent spawn, auth, skills)
- `runtime/handoff.rs`: handoff generation handlers
- `runtime/thread_title.rs`: auto-title handlers

### Feature slices (`src/features/`)

- `features/auth/`: auth feature slice
- `features/input/`: input feature slice (`text_buffer.rs` cursor editing)
- `features/statusline/`: debug status line state/render
- `features/thread/`: thread picker + thread tree view
- `features/transcript/`: transcript feature + markdown rendering

### Other modules

- `src/common/`: shared leaf types
- `src/overlays/`: command palette, skill picker, rename overlays

## Conventions

- Keep Elm/MVU boundaries clear: state/update/render/effects separated.
- Prefer pure render/update functions; isolate side effects in runtime handlers.

## Checks

- Targeted: `cargo test -p zdx-tui`
- Workspace lint/test: use `just lint` / `just test` from repo root

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.
- Architecture changes: update `docs/ARCHITECTURE.md`.