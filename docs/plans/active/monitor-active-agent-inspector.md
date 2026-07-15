> Stage: drafts | active | done | archived. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

> **Status: DRAFT — exploratory, not committed.** Direction from a chat with Talles: "while on the Active Agents tab, press Enter and see what's happening in that session." Reviewed by oracle (2026-07-15): verdict "build with changes" — corrections folded in below. Nothing here should be built until this banner is removed.

# Goals
- From the monitor's **Active Agents** tab, select a running agent and press `Enter` to open a full-screen transcript overlay for that run.
- The overlay renders the recent turns of the run's persisted thread: user/assistant text, reasoning, tool activity (calls + results), and stop/notice events.
- The overlay is a **checkpoint-refreshed persisted transcript**, not a live token stream: it re-reads the thread `.jsonl` on the monitor's timed tick and follows the newest content. It updates at persistence checkpoints (after each completed tool turn), so it will NOT show token-by-token reasoning/text or a tool "currently running".
- Basic scrolling works from the first version (inspect older content, jump to newest).
- `Esc` (and `q`) closes the overlay back to the Active Agents list.

# Non-goals
- A full-fidelity replica of the chat TUI transcript renderer (markdown, syntax highlighting, streaming deltas).
- **True live streaming** of in-progress reasoning/text/tool-input. The persisted JSONL is batched via `TurnCheckpoint`/`TurnFinished`, so disk reads cannot show sub-checkpoint activity. True streaming would require an IPC/event feed and is deferred (see Later).
- Interacting with the run (cancel, inject input, steer). Read-only inspection only.
- Inspecting finished threads from the **Threads** tab (separate later idea; same overlay could be reused).
- Any change to how runs or transcripts are persisted.

# Design principles
- User journey drives order: unlock "press Enter → see the session" first, refine rendering after.
- Reuse before rebuild:
  - Active-agent markers carry `thread_id: Option<String>` (`agent_activity::RunRecord`); threads persist as `.jsonl` under `paths::threads_dir()`.
  - Parse the canonical schema, not ad-hoc JSON: deserialize lines into `thread_persistence::ThreadEvent` (`crates/zdx-engine/src/core/thread_persistence/event.rs`) and reuse `thread_persistence::format_transcript()` (`.../format.rs`) for the MVP render.
  - Mirror the existing overlay **state + key** pattern (Logs overlay: `log_overlay_open`, `handle_logs_key`, early-routed in the dispatcher), but render **full-screen** over `f.area()` with `Clear` (the Logs overlay is a centered 80×60 box, not full-screen).
- Ugly-but-functional first, alpha-stage simple: whole-file reread once per tick is fine; no seek/tail tailing until profiling shows a stall.
- Single responsibility: the overlay only reads the transcript file; it never writes anything.

# User journey
1. User runs `zdx monitor`, switches to the **Active Agents** tab, sees runs shown as `provider:model@thinking` with PID/thread/uptime.
2. User selects a run and presses `Enter`.
3. A full-screen overlay opens showing that run's recent transcript: the user's prompt, the assistant's reasoning/text, and the tool calls + results captured at the last checkpoint.
4. The overlay keeps refreshing on the timed tick while the run works, auto-scrolling to the newest content (until the user scrolls up).
5. User scrolls back to inspect, then presses `Esc` to return to the list.

# Foundations / Already shipped (✅)

## Active-agent registry with (optional) thread id
- What exists: `agent_activity::list_active()` returns `RunRecord { pid, started_at, thread_id: Option<String>, surface, model, provider, thinking, kind, parent_thread_id, subagent_name }` (`crates/zdx-engine/src/agent_activity.rs`). The monitor maps these into `ActiveAgentInfo` in `load_active_agents()` (`app.rs`), currently **truncating `thread_id` to 8 chars** for display.
- ✅ Demo: with a run active, the Active Agents tab lists it with PID + short thread id.
- Gaps: need the **full** id to locate the `.jsonl`. `thread_id` is genuinely `Option` — tracked no-thread/no-save execs can have none — so carry `full_thread_id: Option<String>`, not a `String` + empty sentinel.

## Thread transcripts on disk (canonical `ThreadEvent` JSONL)
- What exists: threads persist as `<thread_id>.jsonl` under `paths::threads_dir()`, one `ThreadEvent` per line. Variants in `event.rs` include message, reasoning (`text: Option<String>` — `None` = redacted/unavailable), tool use, tool result (`{ tool_use_id, output, status }`, no tool name), interrupted, notice, plus meta/usage. Appends are append-mode + newline-delimited (`storage.rs`); the canonical reader already skips unparseable lines best-effort.
- ✅ Demo: `head` a thread `.jsonl` shows one JSON object per line with a `type` field.
- Gaps: no monitor-side reader returning the ordered transcript (only `read_thread_meta` reads the first line). Reuse `format_transcript()` or a small typed pass over `ThreadEvent`.

## Persistence timing (important expectation)
- What exists: `ThreadEvent::from_agent()` documents that streaming text/reasoning/tool-input are batched through `TurnCheckpoint`/`TurnFinished`; checkpoints land after completed tool turns (`agent.rs`). The activity marker guard drops inside `run_turn_inner()` (`agent.rs`), but exec appends the final assistant answer *after* that (`crates/zdx-cli/src/modes/exec.rs`).
- Consequences baked into the plan: (a) overlay updates between tool turns, not within them; (b) "run ended" (marker gone) must NOT stop reads — the final message can still be arriving.

## Full-screen overlay pattern + timed tick
- What exists: Logs overlay state/keys (`log_overlay_open`, `handle_logs_key`) routed **before** section/generic dispatch (early check in the key handler); mouse handling suppressed while it's open. `refresh_app()` re-runs loaders including `app.active_agents = load_active_agents()`.
- ⚠️ Gotchas confirmed by review: `render_log_overlay()` uses `centered_rect(80, 60, area)` (NOT full-screen). `refresh_app()` runs after **every key press** *and* on the 1s tick — so transcript reload must live in the **timed branch only**, not in `refresh_app()`.
- ✅ Demo: on the Logs tab, `Enter` opens the overlay, `Esc` closes it.

# MVP phases (ship-shaped, demoable)

## Phase 1: Full-screen transcript overlay on Enter (static, scrollable) — ✅ done 2026-07-15
- **Goal**: Selecting an active agent and pressing `Enter` opens a full-screen, scrollable overlay showing that run's persisted transcript as of open time.
- **Scope checklist**:
  - [x] Add `full_thread_id: Option<String>` to `ActiveAgentInfo` (`app.rs`); populate from the un-truncated `RunRecord.thread_id`, keeping the truncated `thread_id` for the list display.
  - [x] Model the open overlay as a single `Option<AgentOverlayState>` on `MonitorApp` (captured `thread_id`, resolved path, `pid`/run identity, transcript lines, scroll offset, `follow: bool`, `ended: bool`) rather than a loose boolean + parallel fields. Keeps `#[allow(clippy::struct_excessive_bools)]` from growing and prevents "open with no captured run".
  - [x] Add `read_thread_transcript(path) -> Vec<Line>`: read line-by-line, deserialize each into `thread_persistence::ThreadEvent` (skip a malformed final line), render via `format_transcript()` (MVP) or a compact typed match. Handle: reasoning `None` (skip/label), `Interrupted`, `Notice`. **Truncate every displayed payload** (messages, tool inputs, tool results) — a single record can be huge.
  - [x] `Enter` while `Section::ActiveAgents` and list non-empty: capture identity from the selected row, resolve `threads_dir().join(format!("{id}.jsonl"))`, load transcript, open overlay, scroll to bottom. If `full_thread_id` is `None` → open an "transcript unavailable (no thread id)" state.
  - [x] Route overlay keys **before** section/generic dispatch (beside the existing early log-overlay check) so `Enter`/`Esc`/scroll aren't swallowed; the generic `Enter => toggle_selected_service` no-ops off Services but don't rely on that. Suppress mouse actions while open (extend the existing `log_overlay_open` mouse guard).
  - [x] Basic scroll keys in Phase 1: `j/k`, `Up/Down`, `PageUp/PageDown`, `G`/`End`. Re-clamp scroll against the actual overlay area on resize (mirror the Logs render clamp).
  - [x] Render full-screen over `f.area()` with `Clear` (NOT `centered_rect`); title shows `provider:model@thinking` + short thread id. Gate on the overlay being open while `Section::ActiveAgents`.
  - [x] `Esc`/`q` clears the overlay state.
  - [x] Missing/empty transcript file → overlay opens with "No transcript yet for this run."
- **✅ Demo**: start a long run (`zdx exec` with a real task) in another terminal; in `zdx monitor` Active Agents, `Enter` opens a full-screen overlay showing the prompt + reasoning + tool calls/results captured at the last checkpoint; scroll up/down works; `Esc` returns to the list.
- **Risks / failure modes**:
  - No thread id → dedicated unavailable state (don't crash / don't guess a path).
  - Huge transcript → cap **retained/rendered** records to the last N (e.g. 200) *and* truncate each payload (the cap bounds render, not I/O).
  - Partial final line (live append) → skip on parse error, retry next tick.

## Phase 2: Checkpoint refresh + follow — ✅ done 2026-07-15
- **Goal**: While the overlay is open, it re-reads the transcript on the **timed tick** and follows newest, so the user watches the run advance checkpoint-to-checkpoint.
- **Scope checklist**:
  - [x] Add `refresh_agent_overlay()` and call it **only from the timed tick branch** (not `refresh_app()`), when the overlay is open. Enter does the initial load directly.
  - [x] Re-read is bound to the overlay's captured `thread_id`/path, never the live selection index.
  - [x] Skip reparse when unchanged: store file length + mtime; only reread when they change.
  - [x] Follow behavior: auto-scroll to bottom when already at bottom; scrolling up pauses follow; `G`/`End` resumes at newest (reuse the `log_follow` idea).
  - [x] "Ended" handling: when the captured thread id leaves `list_active()`, set `ended = true` and change only the **title** ("run ended") — **keep refreshing** (final assistant message may still be persisting). Optionally stop once file len/mtime is stable for a few ticks.
- **✅ Demo**: with the overlay open on an active run, new tool turns appear on their own within a tick or two; scrolling up pauses follow; `G` resumes at newest; when the run finishes the title flips to "ended" and the final assistant answer still lands before content settles.
- **Risks / failure modes**:
  - Reload placed in `refresh_app()` → reparses on every keypress; MUST be timed-branch only.
  - Stopping reads on marker removal → loses the final answer; keep reading while open.

# Contracts (guardrails)
- Strictly read-only: never writes, moves, or deletes any thread file or marker.
- The Active Agents list display is unchanged when the overlay is closed (`provider:model@thinking` line intact).
- Overlay open/close must not disturb the Logs overlay or other tabs.
- Missing/empty/mid-write transcript never crashes or hangs — degrade to a message and continue.
- Transcript reload happens **at most once per timed tick**, never per keypress and never per render frame.
- "Run ended" changes presentation only; it never stops reads while the overlay is open.
- Every displayed payload is length-truncated.

# Key decisions (decide early)
- **`full_thread_id` is `Option<String>`** (not `String` + sentinel) — added in Phase 1; Phase 2's reread depends on a stable captured id.
- **Parse canonical `ThreadEvent`, reuse `format_transcript()`** for MVP rather than ad-hoc `serde_json::Value` matching or a bespoke renderer. Custom styled `TranscriptLine` is a polish step, not MVP.
- **Semantics = checkpoint-refreshed, not stream-live.** Documented in Goals/Non-goals; demo copy must not promise a live stream. If in-progress tool status becomes mandatory, that's a rework around an IPC/event feed (Later), not this disk design.
- **Refresh only from the timed tick branch**, because `refresh_app()` also runs on every keypress.
- **Full-screen render** over `f.area()` with `Clear`, not the Logs `centered_rect(80,60)` geometry.
- **Overlay key = `Enter`** (consistent with Logs). `Enter` is not otherwise bound on the Active Agents tab.
- **Scrolling is Phase 1**, not deferred — a bottom-pinned no-scroll view can't inspect history (the stated MVP value).

# Testing
- Manual smoke demo per phase (see each ✅ Demo).
- `cargo nextest run -p zdx-monitor` after changes; `just ci-fast` for lint.
- Minimal regression test for the pure logic: `read_thread_transcript` against a small fixture `.jsonl` — typed `ThreadEvent` lines → expected display lines, including a tolerated malformed final line, a `None` reasoning, an `Interrupted`, and payload truncation. Avoid TUI-render tests.

# Polish rounds (after MVP)

## Polish round 1: Readability
- Color/emphasis per event kind (user vs assistant vs tool vs notice); map `tool_use_id → tool name` so results show their tool; clearer truncation markers.
- ✅ Check-in demo: a busy run is easy to skim — you can tell what the assistant last did and which tool produced which result.

## Polish round 2: Context in the title bar
- Show model, provider, thinking level, uptime, and a running token/usage total (from `usage` events) in the overlay title.
- ✅ Check-in demo: header reflects `provider:model@thinking` plus a token total that grows across checkpoints.

# Later / Deferred
- **True stream-live view (IPC/event feed)** so in-progress reasoning/text/tool status appear before persistence checkpoints. Trigger: checkpoint cadence feels too coarse in daily use.
- **Drill-in for finished threads from the Threads tab** — same overlay from `Section::Threads`. Trigger: the active-run overlay is proven useful and the render model is stable.
- **Shared transcript renderer with the chat TUI** — ✅ done 2026-07-15. Extracted into the new `zdx-transcript` crate (`build_transcript_from_events`, `HistoryCell`, markdown/wrap/style, `cells_to_lines`); reused by `zdx-tui` and `zdx-monitor`. The monitor overlay now renders formatted markdown/wrapped/tool-paired output instead of the raw one-line-per-record version.
- **Incremental seek/tail reads** instead of whole-file rereads. Trigger: profiling shows a real stall on large transcripts.
