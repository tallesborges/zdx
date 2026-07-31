> Stage: done — all 5 MVP phases shipped 2026-07-31 (see each phase's ✅ DONE note for what landed, what the live demos showed, and the defects found along the way). The two polish rounds and the Later/Deferred items were never started; pull this plan back to `active/` if you pick them up.

# Goals

- The monitor Logs tab can be narrowed while debugging: by level, by target, by free-text match.
- The monitor Logs tab can show log files other than the newest one (previous days), and control how much history is tailed.
- The log file contains enough events to debug a run end to end: agent turn loop, provider requests/retries, tool execution, and the other high-value engine paths.
- Log events carry context (thread, model, provider, tool, tool_use_id) via `tracing` spans, so a filtered view answers "which thread / which tool / which model" without extra digging.

# Non-goals

- No structured/JSON log format, no log shipping, no external log viewer.
- No changes to `ZDX_LOG` / `EnvFilter` semantics or to the file rotation scheme in `crates/zdx-engine/src/tracing_init.rs:30-65`.
- No per-token, per-SSE-event, or per-bash-chunk logging.
- No instrumentation of `zdx-tui`, `zdx-transcript`, `zdx-types`, or `zdx-monitor` internals.
- No log viewer inside the chat TUI (monitor only).

# Design principles

- User journey drives order: filters land before the extra volume that needs them.
- Reuse before rebuild: filtering input reuses the `ModelPickerState` filter pattern (`crates/zdx-monitor/src/app.rs:2489-2604`, `2722-2764`); tool logging reuses the single choke point `ToolRegistry::execute_tool` (`crates/zdx-engine/src/tools/mod.rs:292-325`) instead of touching nine leaf tools.
- Raw lines stay the source of truth. Filtering derives a visible-index list; the detail overlay and `y` copy always operate on the raw line.
- Spans carry identity; events stay short. Prefer one span per boundary over repeating IDs in every event.
- Providers stay engine-independent (`crates/zdx-providers/AGENTS.md:17-25`): provider events rely on the parent span for context, they do not import engine state.

# User journey

1. Something misbehaves. Run `just monitor`, open the Logs tab.
2. Cut the noise: filter to `WARN+`, or type a substring (thread id, tool name, error text) to match.
3. Narrow to a subsystem by target (e.g. the agent loop, a provider, the bot).
4. Read the matching lines; open one with `Enter` for the full untruncated entry; `y` to copy.
5. When the problem is in an earlier session, switch to an older log file and raise the tail size.
6. The lines actually explain the run: turn started with model X on thread Y, provider attempt 1 failed and retried, tool `bash` ran and failed with Z.

# Foundations / Already shipped (✅)

## Log file writing + rotation
- What exists: `tracing_init::init` (`crates/zdx-engine/src/tracing_init.rs:30-65`) installs a daily rolling appender at `~/.zdx/logs/zdx.YYYY-MM-DD.log`, compact format, no ANSI, filter from `ZDX_LOG` (default `info`), plus optional `warn+` stderr layer. It is initialized for every binary from `crates/zdx-cli/src/cli/mod.rs:799-805`.
- ✅ Demo: `ZDX_LOG=debug just bot` then check `~/.zdx/logs/` has today's file growing.
- Gaps: none. Do not rebuild this.

## Logs tab tailing, navigation, detail overlay
- What exists: `load_logs`/`tail_lines` (`app.rs:1013-1061`) pick the newest file in `~/.zdx/logs` by mtime and tail the last 256 KiB capped to `LOG_TAIL_LINES = 500`. `handle_logs_key` (`app.rs:1254-1310`) provides `j/k`, arrows, `PageUp/PageDown`, `G`/`End`, follow mode, and `Enter` to open the overlay; overlay keys at `app.rs:1312-1336` (`Esc`/`q`/`Enter` close, `y` copy). Rendering + level coloring in `ui.rs:977-1053`, `1297-1408`.
- ✅ Demo: open the Logs tab, navigate, `Enter` a line, `y` copies it.
- Gaps: no filtering, single file only, fixed tail size, and the parser breaks once spans exist (see Key decisions).

## Reusable filter-input pattern
- What exists: `ModelPickerState { filter, items, matches, selected }` with `recompute()` doing case-insensitive substring matching and selection clamping (`app.rs:2489-2604`), driven by `handle_model_picker_key` (`app.rs:2722-2764`: `Esc` cancel, arrows select, `Backspace` pop, `Char(c)` append, `Enter` commit). Overlay rendering at `ui.rs:1168-1242`.
- ✅ Demo: open the model picker in the Config tab and type to filter.
- Gaps: it is picker-specific; the Logs filter adapts the same shape rather than introducing a new input widget.

## Existing structured-logging style
- What exists: fields-then-literal-message convention, e.g. `agent.rs:977-983` (`tracing::warn!(attempt, max = MAX_RETRIES, delay_ms = delay, error = %retry_err.message, "Transient provider error, retrying")`) and `zdx-bot/src/handlers/message/mod.rs:77-82`.
- ✅ Demo: grep `tracing::` in `zdx-bot` and compare shapes.
- Gaps: only ~136 call sites total, concentrated in `zdx-bot` (101). Zero spans anywhere.

# MVP phases (ship-shaped, demoable)

## Phase 1: Span-aware line parsing + level filter + text search — ✅ DONE 2026-07-31

- **Goal**: the Logs tab can be cut down to what matters, and the parser survives span prefixes.
- **Scope checklist**:
  - [x] Moved log-line parsing out of `ui.rs` into a new `crates/zdx-monitor/src/log_line.rs` (shared by renderer + filter state) and taught it the compact span scope prefix: `<timestamp> <LEVEL> <span1>:<span2>: <target>: <message> <span_fields>`. The whole span scope is one whitespace token, since `FmtCtx` writes `name:` per span with no separating space.
  - [x] Added `spans` to `LogParts` and colored it blue in `log_line_spans`, keeping the existing level/target/message colors.
  - [x] Discriminating target from message required a stricter rule than "ends with `:`": a message word like `failed:` is indistinguishable from a crate-root target. A target is now a token containing `::`, and a third token containing `::` is always the target. Tradeoff (documented at the `is_module_path` call site): a single-segment target such as `zdx_bot:` emitted from inside a span is read as the target rather than as a span scope — the safer misread, since mistaking a message word for the target would corrupt most lines. Real logs confirm `zdx_bot:` crate-root targets exist.
  - [x] Added Logs filter state on `MonitorApp`: `log_visible: Vec<usize>`, `log_level_filter: LevelFilter`, `log_query: String`, `log_query_editing: bool`.
  - [x] Added pure helpers `visible_log_indices` and `clamp_log_view`, wrapped by `recompute_log_visible`.
  - [x] Reindexed `handle_logs_key`, `switch_section`, `refresh_app`, `copy_selected_log_entry`, `render_logs`, and `render_log_overlay` onto `log_visible`; added `MonitorApp::selected_log_line()` as the single raw-line accessor.
  - [x] Bound `l` to cycle ALL → DEBUG+ → INFO+ → WARN+ → ERROR → ALL with a `set_status` confirmation reporting `N of M lines`.
  - [x] Bound `/` to query-edit mode, dispatched before `handle_logs_key` so a typed `q` cannot quit the monitor. `Char` appends, `Backspace` pops, `Enter` accepts, `Esc` clears. Recomputes per keystroke.
  - [x] Bound `Esc` outside edit mode to clear all Logs filters.
  - [x] Title shows `lvl=…`, `/query` (with a cursor block while editing), and `FOLLOW`; the footer shows a live `search: …` line while editing; Logs footer hint gained `/ search • l level`.
  - [x] Distinct empty states: "file is empty / no log files" vs "No lines match <filter> of N tailed lines".
  - [x] Detail overlay and `y` copy read the raw line through `selected_log_line()`.
- **✅ Demo**: verified against real `~/.zdx/logs` via VHS screenshots. `l`×4 → title `lvl=ERROR · 198/198`, footer `Level filter: ERROR (198 of 343 lines)`, only red ERROR rows. `/reply` → title `47/47 · lvl=DEBUG+ · /reply▌`, footer `search: reply▌ (Enter accept · Esc clear)`, a full page of matches. `Esc` → `343/343`, follow re-enabled, `Search cleared`.
- **Bug found and fixed during the demo**: with a stale `log_offset` from the pre-filter list, `clamp_log_view` collapsed the offset onto the selection and rendered a single row while the title claimed `47/47`. Fixed by clamping the offset to `total - page` before the visibility adjustment; covered by `clamp_keeps_page_full_after_a_filter_narrows_the_list`.
- **Tests added**: `log_line::tests` (7: plain/single-span/nested-span/message-colon/span-with-multi-segment-target/unstructured, level cycle + accepts, combined matching) and `app::log_view_tests` (5: level indices, case-insensitive query incl. searching the span scope, follow clamp, shrink clamp, offset scrolling).
- **Verification**: `cargo nextest run -p zdx-monitor` (33 passed), `just ci-fast` clean.
- **Note for Phase 2**: real log files are named `zdx.log.YYYY-MM-DD` (appender prefix + date suffix), not `zdx.YYYY-MM-DD.log`. `~/.zdx/logs` also holds unrelated files such as `automations-daemon.log`, so the file list needs a `zdx.log.*` filter and date-suffix sorting.

## Phase 2: Target filter, file/day switching, tail size — ✅ DONE 2026-07-31

- **Goal**: narrow by subsystem and reach past sessions.
- **Scope checklist**:
  - [x] Added `log_target_filter: Option<String>` to the filter set. Matching is prefix-on-segment-boundary (`line_target` + `target_matches` in `log_line.rs`), so `zdx_engine` selects `zdx_engine::core::agent` but not a hypothetical `zdx_engine_extra`.
  - [x] Bound `f` to a target picker (`TargetPickerState`) reusing the model-picker shape: all items, typed filter, derived `matches`, `Esc`/arrows/`Backspace`/`Char`/`Enter`. Items are the distinct targets in the loaded lines with per-target line counts, most frequent first.
  - [x] Replaced the "newest file by mtime" scan with `log_file_list()`: `zdx.log*` only (excludes `automations-daemon.log`), reverse name sort = reverse date sort. Added `log_files` + `log_file_index`.
  - [x] Bound `[` (older) / `]` (newer) via `switch_log_file`, which resets follow, reloads, and reports `Log file: <name> [i/n]`; at either end it says `Oldest/Newest log file` instead of silently doing nothing.
  - [x] Replaced the fixed tail with `log_tail_lines` cycled by `L` (500 → 2000 → 10000) and scaled the read window to `max_lines × 512 B`, floor 256 KiB — the default tail keeps the historical 256 KiB window.
  - [x] Added `LoadedLogFile` (path + len + mtime + tail size) so `load_active_log` re-tails only when the file identity changes. Older files therefore load once; the live file re-tails only when it actually grows.
  - [x] Title shows the file position `[i/n]` (when more than one file), `@target`, and `tail=N` (when not the default); `Esc` now clears the target filter too; footer hint updated with `f target • [ ] file • L tail • Esc clear`.
  - [x] Target picker is dispatched before the query editor and `handle_logs_key`; `handle_logs_key` was split into `handle_logs_filter_key` + `handle_logs_nav_key` to stay within the clippy function-length limit.
- **✅ Demo**: verified against real `~/.zdx/logs` (149 rolling files) via VHS screenshots. `f` → picker listing 8 targets with counts (`zdx_bot (208)`, `zdx_bot::handlers::message (55)`, …); typing `engine` narrows it; `Enter` applies `@zdx_bot` → title `346/346 · @zdx_bot`, status `Target filter: zdx_bot (346 of 350 lines)`. `[` → `zdx.log.2026-07-30 [2/149] · 500/500`, status `Log file: zdx.log.2026-07-30 [2/149]`. `L` → title gains `tail=2000`.
- **Tests added**: target filter composition with level, target picker frequency ordering + typed filtering, `is_zdx_log_name` selection, `target_matches` segment boundaries, `line_target`, and a `tail_lines` window test on a ~570 KiB fixture proving a 2000-line tail reaches past the 256 KiB default window.
- **Verification**: `cargo nextest run -p zdx-monitor` (40 passed), `just ci-fast` clean.
- **Note**: the initial tail-scaling factor (1 KiB/line) silently doubled the default window and made the first version of the window test vacuous (both cases read the whole fixture). Corrected to 512 B/line with a fixture large enough to distinguish the two windows.

## Phase 3: Core engine spans (agent loop, retries, tool execution) — ✅ DONE 2026-07-31

- **Goal**: a single run is legible in the log: which thread, which model, which turn, which tool, and what failed.
- **Scope checklist**:
  - [x] `#[instrument(name = "turn", skip_all)]` on `run_turn_inner` with `thread`, plus `model`/`provider` recorded after `build_run_turn_setup` (they aren't known at entry). `Turn started` at `info` carries thinking level, tool count, activity kind, and subagent name.
  - [x] Added a local `model_turn` counter in the outer loop; `Turn finished` reports `model_turns`.
  - [x] `Provider request attempt` at `debug` with `model_turn` + `attempt` inside the retry loop; the existing retry `warn` was left as-is.
  - [x] `#[instrument(name = "provider_request")]` on `request_stream`; `consume_stream` logs only stream *ends* — `Provider stream finished` (`debug`, elapsed + stop reason) and `Provider stream failed` (`warn`, elapsed + truncated error). Nothing per SSE event.
  - [x] `#[instrument(name = "tool_turn")]` on `process_tool_turn` + an `Executing tool calls` debug with executable/malformed counts and the tool names.
  - [x] `#[instrument(name = "tool", skip_all, fields(tool, tool_use_id))]` on `ToolRegistry::execute_tool`. The nine `zdx-tools` leaf tools were left untouched.
  - [x] Tool outcome branches on the `ToolOutput` variant: `Failure` → `warn` with `code`, truncated `error`, `duration_ms`; `Canceled` → `debug`; `Success` → `debug` with `duration_ms`. Added `truncate_tool_error` (300 B, char-boundary safe).
  - [x] Disabled tool → `warn` "Tool call rejected: not enabled for this run"; unknown tool → `warn` "Tool call rejected: unknown tool". Both paths were previously silent.
  - [x] Malformed-tool-loop abort → `warn` with malformed/consecutive counts. The tool task join error already logged at `error`, so it was left alone.
  - [x] Turn outcome in `run_turn_with_cancel`: `warn` with a bounded `TurnError::log_summary()`, except interruptions which log at `debug` (user-initiated, not a defect).
- **Two problems the live run exposed (both fixed)**:
  - **Spans did not propagate into tool execution.** Tokio tasks don't inherit the current span, so every tool log line would have lost its turn/thread context. Fixed by attaching `tracing::Span::current()` to the spawned future with `.instrument(...)`; verified by the `turn:tool_turn:tool:` prefix in real output.
  - **`ZDX_LOG=debug` was unusable**: ~70 new lines per run, ~50 of them `h2`/`hyper`/`reqwest`/`ignore`/`globset` internals. `tracing_init` now pins `NOISY_DEPENDENCY_TARGETS` to `warn` unless `ZDX_LOG` names the crate explicitly, so `ZDX_LOG=debug,h2=debug` still works when the transport itself is suspect. Same run afterwards: 8 lines, all ZDX. Note this touches `tracing_init`, which the Non-goals excluded from *semantic* changes — the `ZDX_LOG` contract and the `info` default are unchanged; only third-party verbosity is capped.
  - Also fixed a duplicate `name=` field on tool lines, caused by `skip(...)` not covering the `name` argument (switched to `skip_all`).
- **Deviation**: no separate span on `execute_tools_async`. It is called 1:1 from `process_tool_turn`, so a second span would nest without adding information; the tool list is logged in `tool_turn` instead.
- **✅ Demo**: `ZDX_LOG=debug zdx exec -p "Read /tmp/nope-zdx-phase3.txt then say what happened"` produced exactly the intended trace, and `/turn` in the monitor Logs tab shows it end to end:
  ```
  INFO  turn: Turn started thinking=medium tools=16 kind=main thread=cdfbe6a1… model=claude-opus-5 provider=claude-cli
  DEBUG turn: Provider request attempt model_turn=1 attempt=0
  DEBUG turn: Provider stream finished elapsed_ms=3168 stop_reason=tool_use
  DEBUG turn:tool_turn: Executing tool calls executable=1 malformed=0 tools=read
  WARN  turn:tool_turn:tool: Tool failed duration_ms=1 code=path_error error=Path does not exist '/tmp/nope-zdx-phase3.txt' tool=read tool_use_id=toolu_01AJ…
  DEBUG turn: Provider request attempt model_turn=2 attempt=0
  DEBUG turn: Provider stream finished elapsed_ms=3694 stop_reason=end_turn
  INFO  turn: Turn finished model_turns=2
  ```
  Filtering to `WARN+` yields exactly one line for the failed tool, with tool name, error code, and truncated message — the Phase 3 contract.
- **Tests added**: `tracing_init::tests` — noisy deps pinned to `warn` under `ZDX_LOG=debug`, and an explicit `h2=debug` not being overridden. Written against a pure `build_file_filter(&str)` so no test mutates process env.
- **Verification**: `cargo nextest run -p zdx-engine -p zdx-monitor -p zdx-tools` (676 passed), `just ci-fast` clean, plus the live run above.

## Phase 4: Provider request/response events — ✅ DONE 2026-07-31

- **Goal**: see the outbound request, its status, and its failure per provider.
- **Scope checklist**:
  - [x] Rather than duplicating the same logging in five places, added two helpers to `crates/zdx-providers/src/shared.rs`: `log_request(client, url)` and `check_response_status(client, response)`. The latter *replaces* the identical 4-line status/error-body block each site already had, so the change is net-simplifying rather than additive.
  - [x] Applied at all five request sites: `anthropic/shared.rs` (`send_streaming_request`), `openai/responses.rs` (`send_responses_stream`), `openai/chat_completions.rs`, `gemini/api.rs`, `gemini/antigravity.rs`.
  - [x] Threaded a `client` label through the two shared helpers so callers are distinguishable: `anthropic` vs `claude-cli`, and `openai` / `codex` / `xai` / `grok-build` over the shared Responses path.
  - [x] Events: `Provider request sent` (`debug`, client + endpoint) and `Provider accepted request` (`debug`, status) / `Provider request failed` (`warn`, status + truncated body). Providers stay engine-independent and rely on the parent `turn`/`provider_request` spans for thread/model context.
  - [x] Made the six `subscription_quota` debug lines structurally consistent (`provider` + `error =` fields, capitalized stable messages).
- **Secret-safety measures (the main risk in this phase)**:
  - [x] `endpoint_label()` logs only host + path, deliberately dropping the scheme, userinfo, and query string — a configured base URL can carry `user:pass@`, and some providers accept `?key=`. Tested with all three shapes.
  - [x] No headers, no request bodies. Error bodies are truncated to 300 B.
  - [x] Verified live with a bogus `OPENAI_API_KEY`: the full key appears **0 times** in the log file (OpenAI's own error body masks it, and we never log the key ourselves).
- **Two defects the live failure run exposed (both fixed)**:
  - **Multi-line log events.** Provider error bodies are pretty-printed JSON, so the first version split one event across ~9 log lines — every continuation line then parses as unstructured in the monitor, defeating Phase 1/2 filtering. `truncate_body` now collapses whitespace to a single line. The same hazard existed in `TurnError::log_summary`, fixed the same way.
  - **`Turn failed` had no context.** It is emitted in `run_turn_with_cancel`, outside `run_turn_inner`'s `turn` span, so it carried no thread. Now logs `thread` explicitly.
- **Simplification vs the original checklist**: the events omit `model`. The engine's `turn` span already prints `model` on every line inside a turn, so including it duplicated ~25 characters per line in a fixed-width TUI. `client` + `endpoint` is the information the span does *not* carry.
- **✅ Demo**: `ZDX_LOG=debug` with a bad key produces exactly:
  ```
  DEBUG turn:provider_request: Provider request sent client=openai endpoint=api.openai.com/v1/responses thread=252b5766… model=gpt-5.2 provider=openai
  WARN  turn:provider_request: Provider request failed client=openai status=401 error={ "error": { "message": "Incorrect API key provided: sk-bogus****-log. …
  WARN  Turn failed thread=252b5766… error=provider: HTTP 401: Incorrect API key provided: …
  ```
  A successful run shows `Provider request sent` → `Provider accepted request status=200` → `Provider stream finished elapsed_ms=…`.
- **Tests added**: `shared::request_log_tests` — `endpoint_label` drops query strings (`?key=SECRET`) and userinfo (`user:pass@`) and handles unparseable input; `truncate_body` collapses pretty-printed JSON to one line and truncates with a byte count.
- **Verification**: `cargo nextest run -p zdx-providers -p zdx-engine -p zdx-monitor` (753 passed), `just ci-fast` clean, plus the live success and failure runs above.

## Phase 5: Remaining high-value paths — ✅ DONE 2026-07-31

- **Goal**: the paths that currently fail silently become visible.
- **Scope checklist**:
  - [x] Skills: `info` with loaded count + warning count, plus one `warn` per skipped/invalid skill (`load_skills_from_sources_with_filters`).
  - [x] Subagent: `#[instrument(name = "subagent", fields(subagent, model))]` on `run_exec_subagent_with_cancel`, `Subagent started` (prompt size, streaming, timeout) and terminal outcomes — `Subagent finished` (`info`) / `Subagent exited non-zero` / `Subagent turn failed` / `Subagent returned empty output` (`warn`). Covered on **both** the streaming and non-streaming paths.
  - [x] Memory search: `Memory search completed` (`debug`: strategy, source, result count, duration) and `Memory search failed` (`warn`) in `search_memory_collections_with_binary`.
  - [x] Thread search: `debug` on entry with `has_query` + `limit`. Thread *persistence* was left alone: it consumes every agent event and already warns on append/write failures, so anything more would violate the no-per-event contract.
  - [x] Config: `debug` listing the layer paths actually merged (`load_layered`).
  - [x] Background: `Background process started` (bg_id, pid, thread) after successful registration, and `Background kill requested` in `kill_background`.
- **Consolidation done along the way**: this was the third place needing "collapse whitespace + truncate for a log field", after `truncate_body` (providers) and `truncate_tool_error` (engine tools). Replaced all three with one pure helper, `zdx_types::log_field(value, max_bytes)`, now also used by `TurnError::log_summary` and the subagent stderr capture. Its 4 unit tests cover multi-line collapsing, truncation with original length, short-value passthrough, and multi-byte char boundaries.
- **Two problems the live runs exposed (both fixed)**:
  - **Blocking tools lost their span.** `Memory search completed` initially had no `thread`/`model` because `execute_blocking` uses `spawn_blocking`, which — like `spawn` in Phase 3 — does not inherit the current span. Fixed by entering `Span::current()` inside the blocking closure. This is now the second instance of the same hazard, which is why it is written down in `crates/zdx-engine/AGENTS.md`.
  - **Mislabeled field.** `strategy` was logging the qmd subcommand (`query`, `search`, `vsearch`) rather than the user-facing strategy. Added `QmdMemorySearchStrategy::label()`; the event now reads `strategy=hybrid`, and the failure event reports both `strategy` and `qmd_command`. Also replaced a misleading `timeout_secs=0` (meaning "no timeout") with `timeout=None`.
- **✅ Demo** (all live, `ZDX_LOG=debug`):
  ```
  DEBUG zdx_engine::config: Loaded config layers layers=/Users/…/.zdx/config.toml
  INFO  zdx_engine::skills: Loaded skills count=33 warnings=0
  DEBUG turn:tool_turn:tool: …qmd: Memory search completed strategy=hybrid source=note results=3 duration_ms=1273 thread=3bffdb2d…
  INFO  turn:tool_turn:tool:subagent: Subagent started prompt_bytes=349 streaming=true timeout=None thread=2d3b310f…
  INFO  turn:tool_turn:tool:subagent: Subagent finished output_bytes=1 thread=2d3b310f…
  ```
  A subagent run also demonstrates the payoff of span identity: parent and child interleave in one file but stay separable — the child logs its own `turn` span with `kind=subagent subagent=explorer thread=c7bb1919… model=gpt-5.6-terra`, distinct from the parent's `thread=b6a073d6… model=claude-opus-5`.
- **Verification**: full workspace `cargo nextest run` (1477 passed), `just ci-fast` clean, plus the live memory-search and subagent runs above.

# Contracts (guardrails)

- `ZDX_LOG` keeps controlling the file level; default stays `info`; stderr stays `warn+`; the TUI still passes `stderr: false` (`cli/mod.rs:799-805`).
- The Logs detail overlay and `y` copy always yield the untruncated raw line, regardless of active filters.
- Every failed tool call produces exactly one `warn` line — not zero, not one per retry or per leaf tool.
- No log event inside a per-token, per-SSE-event, or per-bash-output-chunk path (`agent.rs:1473-1497`, `agent.rs:1513+`, `tools/mod.rs:386-411`).
- No secrets, auth headers, full prompts, full messages, or full tool inputs in any event or span field.
- Existing Logs keybindings keep their meaning (`j/k`, arrows, `PageUp/PageDown`, `G`/`End`, `Enter`, `y`); new bindings must not shadow the generic dispatcher (`q`, `r`, `Ctrl+R`, `Tab`/`BackTab`).
- Providers gain no dependency on engine types (`crates/zdx-providers/AGENTS.md:17-25`).

# Key decisions (decide early)

- **Filter model**: derived `log_visible: Vec<usize>` over immutable raw lines, mirroring `ModelPickerState.matches`. Decided — it keeps the overlay/copy contract and avoids a second copy of the text. All selection/offset/follow logic becomes visible-index based, which is the bulk of Phase 1's risk.
- **Compact span prefix parsing**: `parse_log_line` must consume the leading `span_name:` tokens before the target. Verified against `tracing-subscriber-0.3.19` (`FmtCtx` writes span names joined by `:`; `Format<Compact>` writes the target after it and appends span fields after the message). This must land in Phase 1 or every line goes structured-parse-wrong the moment Phase 3 ships.
- **One tool span at the registry choke point**, not nine leaf tools. Decided — `ToolRegistry::execute_tool` already has name, `tool_use_id`, and thread context.
- **Level of new events**: lifecycle at `info`, per-request/per-tool detail at `debug`, failures at `warn`/`error`. Reversing this later means editing every call site.
- **Filter state is session-only** (not persisted to config). Revisit only if a filter is retyped constantly.

# Testing

- Manual smoke demos per phase, as written in each ✅ Demo.
- `cargo nextest run -p zdx-monitor` during iteration; `just ci` as the final gate.
- Minimal regression tests, all pure functions (no new env-var mutation, per workspace test guidance):
  - `parse_log_line` on a span-prefixed compact line, a plain line, a multi-span line, and a non-tracing line.
  - `recompute_log_visible` for level threshold, substring match, target prefix match, and combined filters.
  - Log-file listing/ordering and tail-size capping.
  - Selection/follow clamping when the visible set shrinks to fewer entries than the current selection, and to zero.

# Polish rounds (after MVP)

## Polish round 1: Logs tab performance
- Only re-tail when the active file's mtime/length changed; skip recompute when filters and content are unchanged.
- Incremental append instead of re-tailing the whole window each refresh.
- ✅ Check-in demo: with a 10000-line tail and a busy log, navigation and typing in the query stay responsive.

## Polish round 2: Filter ergonomics
- Highlight the matched substring inside rendered lines.
- Show the resolved filter summary in the footer as well as the title.
- ✅ Check-in demo: type a query and the matched text is visibly highlighted in every rendered line.

# Later / Deferred

- Persisting filter preferences across monitor restarts — revisit if the same filter is retyped every session.
- JSON log output / machine-readable format — revisit if logs need to be consumed by a tool rather than read.
- Regex instead of substring matching — revisit if substring proves insufficient in practice.
- Instrumenting individual `zdx-tools` leaf tools — revisit if the registry-level tool span turns out too coarse to diagnose a specific tool.
- A log viewer inside the chat TUI — revisit only if the monitor stops being the natural place to look.
