# src/ notes

- `src/core/` is the UI-agnostic domain: events, context, interrupt, orchestrator, session.
- `src/core/orchestrator.rs` must not do terminal I/O; it emits events for a renderer/UI to consume.
- `src/ui/transcript.rs` is a TUI view model with styles, wrapping, rendering logic. It stays in UI.
- Output/channel behavior is defined in `docs/SPEC.md`.
- Tools live under `src/tools/` and should stay deterministic (stable envelope, clear errors).
- Keep refactors small and boring; prefer explicit structs and straightforward flow.
