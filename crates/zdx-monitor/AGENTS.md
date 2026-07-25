# zdx-monitor

Compact TUI dashboard for inspecting ZDX services, threads, automations, and config.

## Files
- `src/lib.rs`: crate entry, re-exports `run()`
- `src/app.rs`: app state, event loop, data loading. Tabs are the `Section` enum (exhaustive `ALL`/`next`/`label`/`item_count`). The **Background** tab lists running background processes from `zdx_engine::background_activity` (`load_background()` → `BackgroundInfo`, sorted by thread); `x` kills the selected one (`kill_selected_background`, async on a scratch Tokio runtime).
- `src/ui.rs`: ratatui rendering (`render_background` groups rows under per-thread headers)

## Checks
- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration for this crate: `cargo nextest run -p zdx-monitor`
- Use `just lint` or `just test` only when intentionally running one half of CI
