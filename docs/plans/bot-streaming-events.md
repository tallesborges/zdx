# Goals
- Bot callers get access to streaming `AgentEvent`s during agent execution (tool calls, thinking, deltas)
- `run_agent_turn_with_persist` API is easy to use: caller gets an event stream, persistence is handled internally
- Rename `thread_log` module → `thread_persistence` to better reflect its purpose
- Bot updates Telegram status message with live agent activity (tools running, thinking)

# Non-goals
- Streaming final text to Telegram (Telegram edit rate limits make this impractical for token-by-token)
- Changing the TUI's existing broadcaster wiring
- Adding new event types to `AgentEvent`

# Design principles
- User journey drives order
- Reuse existing `spawn_broadcaster` + `AgentEvent` — no new abstractions
- Caller controls rendering; core controls persistence
- Keep the simple case simple (one function call to get events + persistence)

# User journey
1. Bot receives a Telegram message
2. Bot starts agent turn and gets back an event stream
3. Bot consumes events, updating Telegram status message with activity (🔧 Running bash, 📖 Reading file…)
4. On `TurnCompleted`, bot sends final response
5. Thread persistence happens automatically (caller doesn't manage it)

# Foundations / Already shipped (✅)

## AgentEvent system
- What exists: Full event enum (`ToolRequested`, `ToolStarted`, `ToolCompleted`, `ReasoningDelta`, `AssistantDelta`, `TurnCompleted`, etc.)
- ✅ Demo: TUI renders all these events in real time
- Gaps: Bot doesn't consume them

## spawn_broadcaster
- What exists: `agent::spawn_broadcaster(rx, vec![tx1, tx2])` fans out events to multiple consumers
- ✅ Demo: TUI uses it to fan out to render + persist channels
- Gaps: None

## Thread persistence
- What exists: `spawn_thread_persist_task(thread, rx)` consumes `AgentEventRx` and writes to JSONL
- ✅ Demo: Thread logs are persisted for both TUI and bot
- Gaps: Currently the bot's `run_agent_turn_with_persist` owns the channel exclusively

## Bot cancel support
- What exists: `CancellationToken` + cancel button on "Thinking…" message
- ✅ Demo: Cancel button stops agent, edits message to "Cancelled ✓"
- Gaps: Cancel needs to work with the new spawned-task approach

# MVP slices (ship-shaped, demoable)

## Slice 1: Rename `thread_log` → `thread_persistence`

- **Goal**: Better naming before we change APIs. Pure mechanical rename.
- **Scope checklist**:
  - [x] Rename file `crates/zdx-core/src/core/thread_log.rs` → `crates/zdx-core/src/core/thread_persistence.rs`
  - [x] Update `crates/zdx-core/src/core/mod.rs`: `pub mod thread_persistence;`
  - [x] Add re-export for backward compat: `pub use thread_persistence as thread_log;` (temporary)
  - [x] Update all imports in `zdx-core` (~5 refs)
  - [x] Update all imports in `zdx-tui` (~135 refs) — find-and-replace `thread_log` → `thread_persistence`
  - [x] Update all imports in `zdx-bot` (~6 refs)
  - [x] Update all imports in `zdx-cli` (~19 refs)
  - [x] Update `AGENTS.md` file descriptions
  - [x] Remove the backward-compat re-export once all refs are migrated
- **✅ Demo**: `just ci` passes, all imports use new name
- **Risks / failure modes**:
  - Large find-and-replace — do it in one commit, verify with `just ci`
  - Don't rename the `ThreadLog` struct yet (that's a separate concern, and `ThreadLog` is still a fine name for the handle)

## Slice 2: New `run_agent_turn_streaming` API in bot agent module

- **Goal**: Add a function that returns `AgentEventRx` instead of awaiting the result. Persistence is wired internally. Existing `run_agent_turn_with_persist` stays unchanged (no breakage).
- **Scope checklist**:
  - [x] Add `run_agent_turn_streaming()` in `crates/zdx-bot/src/agent/mod.rs` that:
    - Creates `agent_tx/rx`, `bot_tx/bot_rx`, `persist_tx/persist_rx`
    - Calls `spawn_broadcaster(agent_rx, vec![bot_tx, persist_tx])`
    - Calls `spawn_thread_persist_task(thread, persist_rx)`
    - Spawns `agent::run_turn(…, agent_tx)` in a `tokio::spawn`
    - Returns `bot_rx` (and the `JoinHandle` for cleanup)
  - [x] Return type: struct `AgentTurnHandle { rx: AgentEventRx, task: JoinHandle<Result<…>> }` — clean handle the caller can consume
- **✅ Demo**: Write a small test or call from bot that consumes events from `bot_rx` and prints them to stderr — see tool calls, thinking, completion
- **Risks / failure modes**:
  - `run_turn` errors need to surface via `AgentEvent::Error` (they already do) — caller must handle both channel close and error events
  - `JoinHandle` must be awaited or aborted on cancel to avoid leaked tasks

## Slice 3: Bot consumes streaming events for live status

- **Goal**: Replace the static "🧠 Thinking…" message with live status updates showing what the agent is doing.
- **Scope checklist**:
  - [x] In `handle_message`, replace `run_agent_turn_with_persist` call with `run_agent_turn_streaming`
  - [x] Add event consumption loop that maps events to status text:
    - `ReasoningDelta` → "🧠 Thinking…"
    - `ToolStarted { name: "bash" }` → "🔧 Running `bash`…"
    - `ToolStarted { name: "read" }` → "📖 Reading…"
    - `ToolStarted { name: "write" }` → "✏️ Writing…"
    - `ToolStarted { name: "edit" }` → "✏️ Editing…"
    - `ToolStarted { name: "web_search" }` → "🔍 Searching…"
    - `ToolCompleted` → update status to show completed tools
    - `TurnCompleted` → break loop, send final response
    - `Error` / channel close → handle error
  - [x] Debounce `edit_message_text` calls — at most once every 2-3 seconds (Telegram rate limit is ~30 edits/min per chat)
  - [x] Wire cancellation: on `CancellationToken::cancelled()`, abort the `JoinHandle` from `AgentTurnHandle`
  - [x] Remove old `run_agent_turn_with_persist` call path (or keep as fallback initially)
- **✅ Demo**: Send a message to the bot that triggers tool use. Telegram status message updates from "🧠 Thinking…" → "🔧 Running bash…" → final response
- **Risks / failure modes**:
  - Telegram `edit_message_text` rate limiting — debounce is critical
  - Status message might be deleted by user — handle edit failures gracefully (already have fallback logic)
  - Long tool chains: accumulate a list of completed tools, show latest active one

## Slice 4: Remove old `run_agent_turn_with_persist`

- **Goal**: Clean up — single code path using streaming API.
- **Scope checklist**:
  - [x] Remove `run_agent_turn_with_persist` from `crates/zdx-bot/src/agent/mod.rs`
  - [x] Verify no other callers
- **✅ Demo**: `just ci` passes, `grep` finds no references to old function
- **Risks / failure modes**:
  - None — this is just cleanup after Slice 3 is stable

# Contracts (guardrails)
- Thread persistence must never be skipped — every agent turn must persist events to JSONL
- Cancellation must still work — cancel button stops agent, shows "Cancelled ✓"
- Final response delivery must not regress — if edit fails, fall back to new message
- Existing TUI event consumption must not change

# Key decisions (decide early)
- `run_agent_turn_streaming` lives in the bot crate (not core) — it's bot-specific wiring of core primitives
- Return a handle struct, not a raw tuple of `(rx, join_handle)` — cleaner API
- Debounce interval: 2-3 seconds (can tune later)

# Testing
- Manual smoke demos per slice
- `just ci` for each slice (lint + existing tests)
- Manual Telegram test: send message that triggers multiple tools, verify live status updates

# Polish phases (after MVP)

## Phase 1: Richer status display
- Show accumulated tool list (✓ bash, ✓ read, 🔧 write…)
- Show tool input preview (e.g., file path being read/written)
- ✅ Check-in demo: Status shows full tool history for a multi-tool turn

## Phase 2: Streaming partial text
- For long responses, periodically edit message with partial text (heavily debounced)
- Only for final assistant text, not mid-tool-loop
- ✅ Check-in demo: Long response streams into Telegram in 2-3 chunks before final

# Later / Deferred
- Rename `ThreadLog` struct → revisit if the naming feels wrong after module rename ships
- TUI refactor to use same `AgentTurnHandle` — TUI already has its own wiring, no urgency
- Exec mode streaming status — different UX, different slice
