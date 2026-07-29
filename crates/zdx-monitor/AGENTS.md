# zdx-monitor

Compact TUI dashboard for inspecting ZDX services, threads, automations, and config.

## Files
- `src/lib.rs`: crate entry, re-exports `run()`
- `src/app.rs`: app state, event loop, data loading. Tabs are the `Section` enum (exhaustive `ALL`/`next`/`label`/`item_count`). The **Background** tab lists running background processes from `zdx_engine::background_activity` (`load_background()` → `BackgroundInfo`, sorted by thread); `x` kills the selected one (`kill_selected_background`, async on a scratch Tokio runtime). The Active Agents transcript overlay (`AgentOverlayState`) keeps the `HistoryCell`s plus a `ToolRef` per tool call (its `line..end` row span), so `Tab`/`n`/`p` highlight a tool, `Enter` opens it, and a left click anywhere in a tool's rows opens it directly (`open_tool_pane_at_row`; the overlay is full-frame, so screen row = content row + 1). The pane itself is `ToolPaneState`, keyed by `tool_use_id` so running tools keep updating. `handle_mouse_event` takes the whole `MouseEvent` because clicks need the row.
- `src/ui.rs`: ratatui rendering (`render_background` groups rows under per-thread headers; `render_tool_pane` renders `zdx_transcript::tool_detail_body`, the same body the chat TUI's tool popup shows)

## Checks
- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration for this crate: `cargo nextest run -p zdx-monitor`
- Use `just lint` or `just test` only when intentionally running one half of CI
