# zdx-monitor

Compact TUI dashboard for inspecting ZDX services, threads, automations, and config.

## Files
- `src/lib.rs`: crate entry, re-exports `run()`
- `src/app.rs`: app state, event loop, data loading. The **Logs** tab keeps raw tailed lines in `log_lines` plus a derived `log_visible: Vec<usize>` of indices passing `log_level_filter` (`l`) and `log_query` (`/`); `log_selected`/`log_offset` are positions in `log_visible`, and `selected_log_line()` is the only raw-line accessor (overlay + `y` copy). `recompute_log_visible` wraps the pure `visible_log_indices` + `clamp_log_view`. Filters are level (`l`), target prefix (`f`, via `TargetPickerState`), and substring query (`/`); `Esc` clears all. `log_files`/`log_file_index` list `zdx.log*` newest-first for `[`/`]` day switching, `log_tail_lines` is cycled by `L`, and `LoadedLogFile` stamps (path+len+mtime+tail) keep `load_active_log` from re-reading an unchanged file each tick. Tabs are the `Section` enum (exhaustive `ALL`/`next`/`label`/`item_count`). The **Background** tab lists running background processes from `zdx_engine::background_activity` (`load_background()` → `BackgroundInfo`, sorted by thread); `x` kills the selected one (`kill_selected_background`, async on a scratch Tokio runtime). The Active Agents transcript overlay (`AgentOverlayState`) keeps the `HistoryCell`s plus a `ToolRef` per tool call (its `line..end` row span), so `Tab`/`n`/`p` highlight a tool, `Enter` opens it, and a left click anywhere in a tool's rows opens it directly (`open_tool_pane_at_row`; the overlay is full-frame, so screen row = content row + 1). The pane itself is `ToolPaneState`, keyed by `tool_use_id` so running tools keep updating. The Threads tab's `i` timing overlay uses the shared `zdx_engine::core::thread_timing` reducer/formatter and keeps selection/filter state untouched. `handle_mouse_event` takes the whole `MouseEvent` because clicks need the row.
- `src/log_line.rs`: `tracing` compact log-line parsing (`LogParts`: timestamp/level/span-scope/target/message) plus the Logs-tab filter primitives (`LevelFilter`, `line_matches`). Shared by `app.rs` (filtering) and `ui.rs` (coloring).
- `src/ui.rs`: ratatui rendering (`render_background` groups rows under per-thread headers; `render_tool_pane` renders `zdx_transcript::tool_detail_body`, the same body the chat TUI's tool popup shows; `render_picker` is shared by the Logs target filter and the Threads project filter)

## Threads tab

Rows come from `zdx_engine::core::thread_index::browse_threads()` (`threads.sqlite`), never a directory scan: kind, project, text filter, ordering, and the 500-row cap are all applied in SQL. Child runs are included and labelled with a badge (`subagent_name` for `subagent`, the suffix for `helper:*`). `t` cycles `ThreadKindFilter`, `p` opens the project picker fed by `browse_projects()`, `/` edits the query, `Esc` clears all, `Enter` opens the transcript overlay, and `o` temporarily restores the terminal to open the raw JSONL in `$VISUAL`/`$EDITOR` (or the system default) before resuming Monitor. The query runs on `Enter` rather than per keystroke — a one-letter FTS prefix matches most of the corpus and would stall the UI. The preview reuses `AgentOverlayState` (a saved thread is an ended run), so tool navigation and the tool pane come for free.

## Services tab

The **Services** tab is a control panel over launchd, not a supervisor. `load_services()` maps `zdx_engine::service::Service::ALL` through `service::state()`, and `Enter`/`r` delegate to `zdx_engine::service::{start,stop,restart}`. Monitor never spawns service processes itself, so restart always picks up `~/.local/bin/zdx` rather than the binary the monitor was launched from. Lifetime (login start, crash restart, `/exit` restart) belongs to launchd; install the agents with `zdx service install`.

## Checks
- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration for this crate: `cargo nextest run -p zdx-monitor`
- Use `just lint` or `just test` only when intentionally running one half of CI
