# zdx-monitor

Compact TUI dashboard for inspecting ZDX services, threads, automations, and config.

## Files
- `src/lib.rs`: crate entry, re-exports `run()`
- `src/app.rs`: app state, event loop, data loading
- `src/ui.rs`: ratatui rendering

## Checks
- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration for this crate: `cargo test -p zdx-monitor`
- Use `just lint` or `just test` only when intentionally running one half of CI
