# src/ notes

- `src/engine.rs` must not do terminal I/O; it emits events for a renderer/UI to consume.
- Output/channel behavior is defined in `docs/SPEC.md`.
- Tools live under `src/tools/` and should stay deterministic (stable envelope, clear errors).
- Keep refactors small and boring; prefer explicit structs and straightforward flow.

