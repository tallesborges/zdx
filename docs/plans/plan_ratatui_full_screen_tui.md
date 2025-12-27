# Plan: Implement Full-Screen TUI (TUI2)

This plan outlines the steps to move from the current inline-viewport TUI to the full-screen alternate-screen TUI (TUI2) as specified in `docs/SPEC.md`.

## Goals
- Full-screen alternate-screen interface using `ratatui`.
- Scrollable, width-aware transcript rendering.
- Real-time streaming of assistant responses.
- Tool execution status indicators within the TUI.
- Persistent TUI state during agent execution.

## Progress Summary

**MVP Slices (Core Functionality):** ✅ Complete
- ✅ Slice 0: Terminal safety + blank screen
- ✅ Slice 1: Input works (even ugly)
- ✅ Slice 2: Send loop (no streaming yet)
- ✅ Slice 3: Streaming (throttled)
- ✅ Slice 4: Scroll (read long answers)

**Enhancement Slices (Better UX):** ⏳ In Progress
- ⏳ Slice 5: Turn lifecycle events (TurnStarted)
- ⏳ Slice 6: Tool output streaming (ToolOutputDelta)

**Polish Phases:** Mostly Complete
- ✅ Phase 1: Transcript foundations (UI-agnostic)
- ✅ Phase 1b: Stable IDs + timestamps
- ✅ Phase 2a: Plain text wrap + scroll
- ⏳ Phase 2b: Markdown rendering (strict subset)
- ⏳ Phase 2c: Selection + copy
- ✅ Phase 3: Tool UI
- ⏳ Phase 4: Streaming fidelity + performance
- ✅ Phase 5: Input polish
- ✅ Phase 6: Default + cleanup

## Design Principles

### Reducer pattern for TUI state
Treat TUI state updates like a reducer:
- `AgentEvent` (and user input events) go into a single `update(state, event)` function
- Rendering reads state only
- Tests become: "given events A, B, C → state matches snapshot"
- Aligns with "resumable conversations" goal (event stream is serializable)

### User journey drives order
Build in order of user journey: start TUI → type → send → see reply → stream → scroll → tools → selection/copy → markdown polish

### Focus model (resolve arrow key conflicts)
Two panes compete for arrow keys: input (cursor movement) and transcript (scroll).

**MVP: "Always input focused" approach**
- Arrow keys always edit input (cursor movement)
- Scroll only with `PageUp/PageDown`, `Home/End` in transcript
- Simple, no mode indicator needed

**Later (if needed): Focus toggle**
- `Tab` toggles focus: `InputFocused` ↔ `TranscriptFocused`
- `Esc` always returns focus to input
- Show focus indicator in status line

### Terminal-reliable keybindings
Many terminals don't send distinct keycodes for Ctrl+Enter.

**Default keymap:**
- `Enter` = send message
- `Shift+Enter` or `Ctrl+J` = insert newline (both work; Ctrl+J is fallback for terminals that don't send Shift+Enter)
- `Ctrl+C` = context-sensitive (see below)
- `Esc` = clear input

**Ctrl+C behavior (progressive):**
1. If agent is generating / tool running → **interrupt/cancel**
2. Else if input is non-empty → **clear input**
3. Else → **quit**

In raw mode, Ctrl+C arrives as a key event (`KeyCode::Char('c')` + control modifier), not SIGINT. Handle both paths.

### Streaming events with stable IDs
Each streaming cell gets a stable ID for reducer determinism:
- `turn_id` — the conversation turn
- `cell_id` — specific cell being streamed
- `tool_call_id` — if tool output appears inline

Events are reducer-friendly:
- `AssistantStart { cell_id }`
- `AssistantDelta { cell_id, text_chunk }`
- `AssistantEnd { cell_id }`
- `ToolStart { cell_id, tool_name }`
- `ToolEnd { cell_id, status }`

This prevents "which cell am I appending to?" bugs.

---

# MVP Slices (each must be demoable)

These slices get to "daily-usable" as fast as possible. Each has a clear demo criterion.

## Slice 0: Terminal safety + blank screen ✅
Goal: Alt-screen + raw mode + restore guard. Never wreck the terminal.

- [x] Add `src/ui/tui2.rs` with `Tui2App` struct.
- [x] Enter alternate screen on start; leave on exit.
- [x] Enable raw mode on start; disable on exit.
- [x] **Guard pattern:** terminal restore via `Drop` impl.
- [x] **Panic hook:** restore terminal before printing panic.
- [x] **Signal handling:** ctrl-c restores terminal cleanly.
- [x] Quit key (`q` or `Ctrl+C`) exits cleanly.
- [x] ✅ **Demo:** `cargo run -- dev tui2` — start/quit never wrecks terminal.

**Implementation notes:**
- `Tui2App` struct in `src/ui/tui2.rs`.
- Hidden `dev tui2` CLI command for testing.
- Uses existing `core::interrupt` module for Ctrl+C handling (global handler already registered in main).
- Panic hook installed before entering alternate screen.
- `Drop` impl ensures terminal restored even on error paths.

## Slice 1: Input works (even ugly) ✅
Goal: Functional input editing. Use `tui-textarea` (already in deps).

- [x] Wire `tui-textarea` into input pane.
- [x] Insert/delete characters.
- [x] Cursor movement: left/right, home/end, up/down (multiline).
- [x] Multiline input support.
- [x] Submit vs newline: Enter submits, Shift+Enter or Ctrl+J for newline.
- [x] Paste handling (terminal paste works via EnableBracketedPaste).
- [x] On submit: create a `User` cell in transcript (in-memory only, no agent yet).
- [x] ✅ **Demo:** `cargo run -- dev tui2` — type/edit/paste, submit shows "You: ..." in transcript pane.

**Implementation notes:**
- Input area at bottom with `tui-textarea`.
- Transcript renders using `HistoryCell::display_lines()` from Phase 1.
- Style conversion from transcript `Style` enum to ratatui styles.
- Escape clears input, q quits (only when input empty).
- Arrow keys always control input cursor (focus model: always input focused).

## Slice 2: Send loop (no streaming yet) ✅
Goal: Actually call the agent and get a response.

- [x] On submit: spawn agent turn (non-blocking).
- [x] Show "thinking..." or spinner while waiting.
- [x] When response arrives: append `Assistant` cell with full text.
- [x] No streaming, no markdown, plain text only.
- [x] ✅ **Demo:** ask a question, get an answer displayed in TUI.

**Implementation notes:**
- `AgentState` enum tracks Idle vs Waiting states.
- Agent task spawned via `tokio::spawn`, polled via `is_finished()`.
- "thinking..." indicator shown in transcript while waiting.
- Error handling: shows error in transcript, removes failed user message from history.
- Ctrl+J added for terminal-reliable newline insertion.

## Slice 3: Streaming (throttled) ✅
Goal: Stream responses smoothly without input lag.

- [x] Create bounded channel from agent to TUI.
- [x] Map `AssistantDelta { cell_id, text_chunk }` events into transcript updates.
- [x] Coalesce rapid deltas (don't redraw per-character).
- [x] Tick-based redraw (e.g., 30fps max during streaming).
- [x] Show streaming cursor (▌) during response.
- [x] ✅ **Demo:** response streams smoothly, typing stays responsive during stream.

**Coalescing strategy:**
- UI loop buffers deltas per streaming cell (`pending_delta: String`)
- On each tick (30fps), reducer applies one combined append and clears pending
- Keeps input responsive and redraw stable
- Streaming cursor width affects wrapping (account for ▌ in unicode-width later)

**Implementation notes:**
- `AgentState` enum: `Idle`, `Waiting` (before first delta), `Streaming` (actively receiving).
- `poll_agent_events()` drains channel with `try_recv()` (non-blocking).
- `apply_pending_delta()` coalesces all buffered text into single append per tick.
- `poll_agent_completion()` handles task finish and message history update.
- 30fps frame rate via `FRAME_DURATION` constant (33ms poll timeout).
- Streaming cell created on first delta, finalized on `AssistantComplete` event.

## Slice 4: Scroll (read long answers) ✅
Goal: Navigate long transcripts.

- [x] Wrap lines at current terminal width (plain text, no markdown yet).
- [x] Flatten wrapped lines into visual-line list.
- [x] Track scroll offset over flattened lines.
- [x] **FollowLatest:** auto-scroll to bottom on new content (default).
- [x] **Anchored:** user scroll (PageUp/Down) switches to anchored mode.
- [x] Press `Ctrl+End` to re-enable follow-latest.
- [x] ✅ **Demo:** long reply, PageUp/PageDown works, auto-scroll resumes.

**Implementation notes:**
- `ScrollMode` enum: `FollowLatest` (default) vs `Anchored { offset }`.
- `cached_line_count` tracks total lines for scroll calculations.
- Header shows "▼ more" indicator when content is below viewport.
- PageUp/PageDown scroll by visible page height.
- Mouse wheel scrolls by 3 lines (via `EnableMouseCapture`).
- Ctrl+Home jumps to top (Anchored), Ctrl+End jumps to bottom (FollowLatest).
- When PageDown or mouse scroll reaches bottom, automatically switches to FollowLatest.

**Anchored mode behavior:**
- When anchored: scroll offset stays fixed; new content extends line count but viewport doesn't move.
- Header shows "▼ more" indicator when content is below viewport.
- `Ctrl+End` jumps to bottom and re-enables FollowLatest.
- Prevents jarring "why did it jump?" experience.

---

At this point, TUI2 is **daily-usable enough to dogfood**.

---

# Enhancement Slices (progressive improvements)

These slices improve the user experience with better feedback and real-time progress indicators. Each builds on the working MVP.

## Slice 5: Turn lifecycle events ⏳
Goal: Show immediate feedback when assistant starts working.

**Problem:** Currently there's a 1-3 second gap between user hitting Enter and the first AssistantDelta arriving. User doesn't know if the request was received.

**Solution:** Emit `TurnStarted` event immediately when `run_turn()` begins.

**Agent changes:**
- [ ] Add `sink.important(AgentEvent::TurnStarted).await;` at start of `run_turn()`
- [ ] Event already defined in `src/core/events.rs` (from naming refactor)

**UI changes:**
- [ ] Update `AgentState::Waiting` to trigger on `TurnStarted` instead of on task spawn
- [ ] Show "Assistant is thinking..." status immediately
- [ ] Optional: Show spinner/animation while waiting for first delta
- [ ] Handle in exec mode (currently no-op, which is fine)

**Session persistence:**
- [ ] Optional: Log turn boundaries in session file for analytics
- [ ] `SessionEvent::from_agent()` can track turn starts if needed

**Testing:**
- [ ] Unit test: `run_turn()` emits `TurnStarted` before any other events
- [ ] Integration test: TUI shows "thinking" indicator within <100ms of Enter

**✅ Demo:** User sends message → "Thinking..." appears instantly → text starts streaming 1-2s later (but user knows work started).

**Effort:** 🟢 Trivial (1-2 hours)
- Agent: 1 line change
- UI: Update existing handler to show spinner
- Tests: Simple event ordering test

**Impact:** 🟢 High (better perceived responsiveness)

---

## Slice 6: Tool output streaming ⏳
Goal: Show real-time progress for long-running tool executions.

**Problem:** When running `cargo test`, `cargo build --release`, or other long commands, the TUI shows "Tool running..." for 30-60 seconds with no feedback. Users don't know if it's frozen or making progress.

**Solution:** Stream tool stdout/stderr chunks via `ToolOutputDelta` events as they arrive.

**Agent changes:**
- [ ] Modify `tools::execute_tool()` signature to accept `on_chunk: impl Fn(String)` callback
- [ ] Update `execute_tools_async()` to wire up callback:
  ```rust
  let (output, result) = tools::execute_tool(
      &tu.name, 
      &tu.id, 
      &tu.input, 
      ctx,
      |chunk| {
          sink.delta(AgentEvent::ToolOutputDelta {
              id: tu.id.clone(),
              chunk,
          });
      }
  ).await;
  ```
- [ ] Bash tool: Stream stdout/stderr line-by-line (or chunk-by-chunk)
- [ ] Other tools: Can use same pattern if they have long output

**Bash tool implementation:**
- [ ] Use `tokio::process::Command` with piped stdout/stderr
- [ ] Read output with `BufReader::lines()` or `BufReader::read_until()`
- [ ] Call callback for each line/chunk
- [ ] Accumulate full output for final `ToolOutput` result
- [ ] Handle timeout with partial output

**UI changes:**
- [ ] Update `AgentEvent::ToolOutputDelta` handler in `update.rs`
- [ ] Find tool cell by ID, append chunk to streaming output
- [ ] Implement output size limit (e.g., keep last 100KB to prevent memory issues)
- [ ] Auto-scroll to bottom when new chunks arrive (if in FollowLatest mode)
- [ ] Show "▼ more" indicator if output exceeds viewport

**Exec mode changes:**
- [ ] Print chunks to stderr immediately (like ToolStarted/ToolFinished)
- [ ] No buffering needed in exec mode

**Session persistence:**
- [ ] ToolOutputDelta is transient (not persisted)
- [ ] Only final ToolFinished result goes to session file
- [ ] Session replay shows final output, not streaming

**Testing:**
- [ ] Unit test: Bash tool calls callback for each line
- [ ] Unit test: UI appends chunks to correct tool cell
- [ ] Integration test: Long command shows incremental output
- [ ] Integration test: Output size limit prevents memory issues

**✅ Demo:** Run `cargo test` → see each test passing in real-time → user knows progress is happening.

**Real-world use cases:**
1. **Builds:** `cargo build --release` shows compilation progress
2. **Tests:** `cargo test` shows each test result as it runs
3. **Search:** `grep -r "pattern" .` shows matches as found
4. **Downloads:** `curl` shows progress chunks

**Effort:** 🟡 Medium (4-8 hours)
- Agent: Modify tool execution signature (2 hours)
- Bash tool: Implement streaming (2 hours)
- UI: Update cell rendering (2 hours)
- Tests: Integration tests (2 hours)

**Impact:** 🟢 High (essential for long-running operations)

**Implementation order:**
1. Start with bash tool only (most common long-running case)
2. Add to read/write/edit later if needed (less critical)

---

# Foundations (completed)

## Phase 1: Transcript foundations (UI-agnostic) ✅
Goal: define the transcript types and rendering hook that everything else builds on.

Deliverables
- [x] Add `HistoryCell` (user/assistant/tool/system) in `src/core/transcript.rs`.
- [x] Add `StyledLine` representation (UI-agnostic).
- [x] Implement `HistoryCell::display_lines(width) -> Vec<StyledLine>` (plain text).

**Implementation notes:**
- `HistoryCell` variants: `User`, `Assistant` (with `is_streaming`), `Tool`, `System`.
- `Style` is semantic enum; renderers translate to terminal styles.
- `display_lines(width)` with word-wrapping, streaming cursors.
- 16 unit tests.

## Phase 1b: Stable IDs + timestamps ✅
Goal: Stable identifiers for selection, scroll anchoring, tool status.

Deliverables
- [x] `CellId(u64)` with atomic counter.
- [x] `created_at: DateTime<Utc>` on all cells.
- [x] `ToolState { Running, Done, Error }` with `started_at`.

**Implementation notes:**
- All cells have `id` and `created_at`.
- Tool cells have explicit state enum.
- 3 new tests.

---

# Polish Phases (after MVP)

## Phase 2a: Plain text wrap + scroll ✅
Goal: Proper line wrapping (already partially done in Slice 4).

- [x] Add `unicode-width` for display width (CJK, emoji).
- [x] Wrap by display width, not byte length.
- [x] Cache wrapped lines per `(cell_id, width)`.
- [x] ✅ Check-in: emoji and CJK characters wrap correctly.

**Implementation notes:**
- Added `unicode-width` crate for display width calculation.
- `wrap_text()` now uses `UnicodeWidthStr::width()` instead of byte length.
- `break_word_by_width()` handles breaking long words at proper character boundaries.
- `render_prefixed_content()` uses display width for prefix/indent calculation.
- `WrapCache` struct with `RefCell` interior mutability for caching during render.
- Cache key: `(CellId, width, content_len)` to invalidate on content changes.
- Streaming/running cells are not cached (dynamic content).
- Cache cleared on terminal resize events.
- 16 new tests for unicode wrapping and caching.

## Phase 2b: Markdown rendering (strict subset)
Goal: Styled markdown output. Keep scope tight.

**Subset (everything else = plain text):**
- paragraphs + soft wrap
- inline code (backticks)
- fenced code blocks
- bold/italic

Deliverables
- [ ] Add minimal markdown parser (or use `pulldown-cmark`).
- [ ] Convert markdown to styled spans.
- [ ] Store raw markdown in cells; render styled at display time.
- [ ] ✅ Check-in: code blocks render with background, bold/italic work.

## Phase 2c: Selection + copy
Goal: Select and copy transcript text.

### Unicode/grapheme decision
- **grapheme index** for selection (user-visible character)
- Add `unicode-segmentation` crate

### Copy transport decision
- [ ] **OSC 52** (terminal clipboard): works in many terminals
- [ ] **System clipboard** fallback (`arboard` crate)
- [ ] **Internal buffer** fallback if both fail

Deliverables
- [ ] Position mapping: `(visual_line, column)` ↔ `(cell_id, raw_text_range)`.
- [ ] Selection overlay on visible lines.
- [ ] Copy reconstructs text with correct wrapping.
- [ ] ✅ Check-in: select text, copy, paste elsewhere matches.

## Phase 3: Tool UI ✅
Goal: Show tool execution in transcript.

- [x] "Tool running..." indicator when tool starts.
- [x] Show tool name and status (running/done/failed).
- [x] ESC or Ctrl+C interrupts running agent/tool.
- [ ] Optionally show tool output preview (deferred).
- [x] ✅ Check-in: tool use shows status in TUI, not stdout.

**Implementation notes:**
- Handle `ToolRequested` → create `HistoryCell::tool_running()` in transcript.
- Handle `ToolFinished` → update tool cell with `set_tool_result()`.
- ESC/Ctrl+C when agent running → `interrupt::trigger_ctrl_c()`.
- Ctrl+C progressive: interrupt → clear input → quit.

## Phase 4: Streaming fidelity + performance
Goal: Stable streaming under resizes and long outputs.

- [ ] Commit cursor: render only committed prefix during stream.
- [ ] Redraw throttling to reduce flicker.
- [ ] Wrap cache invalidation on resize.
- [ ] Avoid whole-transcript rewrap per frame.
- [ ] ✅ Check-in: stream long response, resize terminal, no flicker or lag.

## Phase 5: Input polish ✅
Goal: Quality-of-life input features.

- [x] History navigation (↑/↓ for previous commands).
- [x] Ctrl+C progressive behavior (interrupt → clear → quit, per Design Principles).
- [x] Status line with mode indicator (streaming, focus state if focus toggle added).
- [x] ✅ Check-in: input feels polished for daily use.

**Implementation notes:**
- Command history stored per session, persisted across resumed sessions.
- ↑/↓ navigation when cursor at first/last line or input empty.
- Draft text saved when entering history navigation, restored when exiting.
- Status line shows: model name, state (Ready/Thinking/Streaming), history position when navigating.
- Header height increased to 3 lines (title + status + border).

## Phase 6: Default + cleanup ✅
Goal: TUI2 becomes the default.

- [x] Switch `zdx` interactive path to TUI2 by default.
- [x] Removed `src/ui/app.rs` (inline TUI).
- [x] Integration test: "no stdout transcript while TUI2 is active" (tests/tui_chat.rs).
- [x] Added session persistence support to TUI2.
- [x] Added `with_history` constructor for session resume support.
- [x] ✅ Check-in: TUI2 is now the default for interactive mode.

---

# Later / Explicitly Deferred

These items are **not** in MVP, Enhancement, or Polish phases. Don't build until needed.

- **Selection** — MVP has no selection; when added, use grapheme indices from day 1 (no byte-index intermediate state to undo)
- **Full markdown spec** — lists, tables, links, images
- **Bidirectional position mapping** — only if needed for cursor-follow
- **Mouse selection** — keyboard-only is fine for MVP
- **Focus toggle mode** — start with "always input focused"; add toggle only if needed
- **Tool output preview in cell** — current implementation shows full output; preview/expand could be added later

---

# Minimal transcript model for MVP

You don't need the full visual-line pipeline up front. MVP uses:

```rust
Vec<HistoryCell { id, role, raw_text, is_streaming }>
```

- Wrapping at render time (basic word-wrap)
- Scroll offset over flattened wrapped lines
- Cache per `(cell_id, width)` added in polish phase

This supports long-term architecture without blocking sending messages.
