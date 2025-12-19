# PLAN v0.2.1 — Channel-based EventSink Refactor

> **Goal:** Replace callback-based `EventSink` with `tokio::sync::mpsc` channels to eliminate deadlock risks, enable concurrent rendering, and prepare for TUI.
>
> **Relates to:** ROADMAP "Now" item: *Make engine/renderer separation strict and boring*
>
> **Decision rationale:** See ADR-0003 (channel-based EventSink)

---

## Background

### Current architecture (problematic)
```
Engine (run_turn)
    │
    └── sink(event)  ← sync callback, Box<dyn FnMut>
            │
            ▼
    create_persisting_sink()
        ├── renderer.lock().unwrap().handle_event(event)
        └── session.lock().unwrap().append(...)
```

**Problems identified (via gpt-5.2-codex review):**
1. **High risk:** `Arc<Mutex<CliRenderer>>` held inside sink closure — deadlock if used with bounded channel
2. **High risk:** `Arc<Mutex<Session>>` held inside sink closure — same issue
3. **Medium risk:** Events emitted inline in `run_turn()` — receiver not driven concurrently

### Target architecture
```
Engine (run_turn)
    │
    └── tx.send(Arc<Event>).await  ← async, bounded channel
            │
            ▼
      ┌─────────────┐
      │  Fan-out    │
      │    task     │
      └─────────────┘
            │
      ┌─────┴─────┐
      ▼           ▼
 render_tx    persist_tx
      │           │
      ▼           ▼
 ┌─────────┐ ┌──────────┐
 │Renderer │ │ Session  │
 │  task   │ │   task   │
 │(owns    │ │(owns     │
 │ state)  │ │ state)   │
 └─────────┘ └──────────┘
```

**Benefits:**
- No shared mutexes — each task owns its state
- Backpressure via bounded channels
- Concurrent rendering while engine streams
- Clean shutdown via sender drop
- TUI-ready (swap renderer task)

---

## Step 1: Accept ADR for channel-based EventSink

**Commit:** `docs: accept ADR-0003 for channel-based EventSink`

**Goal:** Document the decision to switch from callback to mpsc channels.

**Deliverable:** `docs/adr/0003-channel-based-eventsink.md` status changed to `Accepted`.

**Files changed:**
- `docs/adr/0003-channel-based-eventsink.md` (status update)

**Verification:** `grep "Status:" docs/adr/0003-channel-based-eventsink.md`

---

## Step 2: Add tokio mpsc channel types to engine

**Commit:** `feat(engine): add channel-based EventTx type alongside EventSink`

**Goal:** Introduce new types without breaking existing code.

**Deliverable:**
- `EventTx = mpsc::Sender<Arc<EngineEvent>>` type alias
- `EventRx = mpsc::Receiver<Arc<EngineEvent>>` type alias
- Helper `fn create_event_channel(capacity: usize) -> (EventTx, EventRx)`

**Files changed:**
- `src/engine.rs`

**Tests:** Existing tests continue to pass (`cargo test`)

**CLI demo:** N/A (internal change)

---

## Step 3: Create async run_turn variant

**Commit:** `feat(engine): add run_turn_async that accepts EventTx`

**Goal:** New engine entry point using channels, coexisting with old callback version.

**Deliverable:**
- `pub async fn run_turn_async(..., sink: EventTx) -> Result<...>`
- All `sink(event)` calls become `sink.send(Arc::new(event)).await`
- Handle send errors as shutdown signal (not panic)

**Files changed:**
- `src/engine.rs`

**Tests:** 
- New test `test_run_turn_async_emits_events` using channel receiver
- Existing callback tests unchanged

**CLI demo:** N/A (not wired yet)

**Edge cases:**
- Channel closed mid-stream → return early, no panic
- Backpressure → engine waits (by design)

---

## Step 4: Create renderer task that consumes from channel

**Commit:** `feat(renderer): add spawn_renderer_task consuming from EventRx`

**Goal:** Renderer runs as independent task, owns its state.

**Deliverable:**
```rust
pub fn spawn_renderer_task(rx: EventRx) -> JoinHandle<()>
```
- Task owns `CliRenderer` (no Arc<Mutex>)
- Loops `while let Some(ev) = rx.recv().await`
- Calls `renderer.handle_event((*ev).clone())`
- Calls `renderer.finish()` on channel close

**Files changed:**
- `src/renderer.rs`

**Tests:**
- Unit test with mock channel: send events, verify task completes

**CLI demo:** N/A (not wired yet)

---

## Step 5: Create session persistence task

**Commit:** `feat(session): add spawn_persist_task consuming from EventRx`

**Goal:** Session persistence runs as independent task, owns Session.

**Deliverable:**
```rust
pub fn spawn_persist_task(session: Session, rx: EventRx) -> JoinHandle<()>
```
- Task owns `Session` (no Arc<Mutex>)
- Filters events → `SessionEvent::from_engine(&ev)`
- Appends to session file

**Files changed:**
- `src/session.rs`

**Tests:**
- Unit test: send tool events via channel, verify session file written

**CLI demo:** N/A (not wired yet)

---

## Step 6: Create fan-out dispatcher task

**Commit:** `feat(engine): add spawn_fanout_task for multi-consumer dispatch`

**Goal:** Single engine channel fans out to renderer + persister.

**Deliverable:**
```rust
pub fn spawn_fanout_task(
    rx: EventRx,
    render_tx: EventTx,
    persist_tx: EventTx,
) -> JoinHandle<()>
```
- Receives from engine
- Clones `Arc<Event>` to both downstream channels
- Exits when engine channel closes

**Files changed:**
- `src/engine.rs` (or new `src/dispatch.rs`)

**Tests:**
- Unit test: send events, verify both receivers get them in order

**CLI demo:** N/A (not wired yet)

---

## Step 7: Wire channel architecture into chat module

**Commit:** `feat(chat): use channel-based engine with renderer/persist tasks`

**Goal:** Replace `create_persisting_sink()` with spawned tasks.

**Deliverable:**
- `run_chat_turn` creates channels, spawns tasks, calls `run_turn_async`
- Awaits all tasks after engine completes
- Remove `Arc<Mutex<CliRenderer>>` usage
- Remove `Arc<Mutex<Session>>` from sink

**Files changed:**
- `src/chat.rs`

**Tests:**
- Existing chat integration tests pass
- Manual test: `cargo run -- chat` works with tool use

**CLI demo:**
```bash
echo "hello" | cargo run -- chat
```

**Edge cases:**
- Interrupt handling still works (Ctrl+C)
- Session still persisted on normal exit

---

## Step 8: Wire channel architecture into agent module

**Commit:** `feat(agent): use channel-based engine with renderer/persist tasks`

**Goal:** Same pattern as chat for `exec` command.

**Deliverable:**
- `run_agent` creates channels, spawns tasks, calls `run_turn_async`
- Remove `create_persisting_sink()` usage

**Files changed:**
- `src/agent.rs`

**Tests:**
- Existing agent tests pass

**CLI demo:**
```bash
cargo run -- exec "say hello"
```

---

## Step 9: Remove old callback-based EventSink

**Commit:** `refactor(engine): remove deprecated EventSink callback type`

**Goal:** Clean up old code path.

**Deliverable:**
- Remove `pub type EventSink = Box<dyn FnMut...>`
- Remove `run_turn` (old callback version)
- Rename `run_turn_async` → `run_turn`
- Remove `create_persisting_sink()` from chat/agent
- Remove `CliRenderer::into_sink()`
- Update all imports

**Files changed:**
- `src/engine.rs`
- `src/chat.rs`
- `src/agent.rs`
- `src/renderer.rs`

**Tests:**
- All tests updated to use channel-based API
- `cargo test` passes

**CLI demo:**
```bash
cargo run -- exec "list files" 
cargo run -- chat
```

---

## Step 10: Consolidate chat loops

**Commit:** `refactor(chat): remove duplicated run_chat, use single run_chat_loop`

**Goal:** Eliminate the `run_chat<R, W>` duplication identified earlier.

**Deliverable:**
- Single `run_chat_loop` generic over I/O
- `run_interactive_chat_with_history` is thin wrapper
- Remove `#[allow(dead_code)]` test-only `run_chat`

**Files changed:**
- `src/chat.rs`

**Tests:**
- Existing tests pass (may need minor updates)

**CLI demo:**
```bash
cargo run -- chat
```

---

## Step 11: Add delta coalescing (optional optimization)

**Commit:** `feat(engine): coalesce AssistantDelta events before sending`

**Goal:** Reduce channel traffic for high-frequency deltas.

**Deliverable:**
- Buffer deltas in engine
- Flush on: time threshold (~20-50ms), size threshold (~100 chars), semantic boundary (tool_use, final)
- Single coalesced delta sent instead of many small ones

**Files changed:**
- `src/engine.rs`

**Tests:**
- Test that multiple rapid deltas result in fewer channel sends
- Test that semantic boundaries force flush

**CLI demo:** Output still streams smoothly

---

## Verification checklist

After completing all steps:

- [ ] `cargo build` succeeds
- [ ] `cargo test` passes
- [ ] `cargo clippy` clean
- [ ] `cargo run -- exec "hello"` works
- [ ] `cargo run -- chat` works with multi-turn
- [ ] Tool use (`read`, `bash`) works
- [ ] Session persistence works
- [ ] Ctrl+C interrupt works
- [ ] No `Arc<Mutex<CliRenderer>>` in codebase
- [ ] No `Arc<Mutex<Session>>` in sink closures

---

## Rollback plan

If issues arise:
1. Steps 1-6 are additive (new code alongside old)
2. Steps 7-8 can be reverted independently
3. Step 9 is the point of no return — ensure CI green before merging
