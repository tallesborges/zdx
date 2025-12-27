# ADR 0003: Channel-based EventSink with concurrent workers

Date: 2025-12-18
Status: Accepted

## Context

ADR-0002 established that the agent emits `AgentEvent` values into a renderer-provided sink. The current implementation uses a callback-based sink:

```rust
pub type EventSink = Box<dyn FnMut(AgentEvent) + Send>;
```

This approach has deadlock risks when combined with bounded async channels:

1. **High risk:** `create_persisting_sink()` in `chat.rs` holds `Arc<Mutex<CliRenderer>>` and `Arc<Mutex<Session>>` inside the closure. If the sink ever does `.await` while holding these locks, a bounded channel will deadlock when backpressure hits.

2. **High risk:** `CliRenderer::into_sink()` wraps the renderer in `Arc<Mutex<_>>`. The receiver task cannot drain events if it needs the same lock.

3. **Medium risk:** Events are emitted inline in `run_turn()`. If the receiver runs on the same task (or only after `run_turn()` returns), `send().await` stalls when the buffer fills.

These issues block progress toward:
- Bounded backpressure (OOM prevention for fast streams)
- TUI readiness (concurrent rendering during streaming)
- Clean async architecture (no mutex locks across `.await`)

## Decision

Replace the callback-based `EventSink` with `tokio::sync::mpsc` bounded channels and concurrent worker tasks:

```rust
pub type EventTx = mpsc::Sender<Arc<AgentEvent>>;
pub type EventRx = mpsc::Receiver<Arc<AgentEvent>>;
```

### Architecture

```
Agent (run_turn)
    │
    └── tx.send(Arc<Event>).await
            │
            ▼
      ┌─────────────┐
      │  Fan-out    │  (spawned task)
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

### Key properties

- **Bounded channels** (~64 capacity) provide backpressure
- **Owned state** — each task owns its renderer/session (no shared mutex)
- **Arc<AgentEvent>** for efficient cloning across tasks
- **Fan-out task** preserves event ordering to all consumers
- **Clean shutdown** via sender drop (tasks exit when channel closes)

### Backpressure strategy

- `send().await` for all events (lossless, applies backpressure)
- Future optimization: coalesce `AssistantDelta` events in producer (flush every ~20-50ms or at semantic boundaries)

## Consequences

### Positive
- Eliminates deadlock risks from mutex + async interaction
- Enables concurrent rendering while agent streams
- Prepares architecture for TUI (swap renderer task)
- Testable via channel inspection (no callback mocking)
- Bounded memory usage under fast streams

### Negative
- Breaking change to `EventSink` type (all callers must update)
- Slight complexity increase (task spawning, channel wiring)
- `Arc<AgentEvent>` adds allocation per event (mitigated by coalescing)

### Migration

1. Add new channel types alongside existing callback
2. Create async `run_turn` variant
3. Wire chat/agent to new architecture
4. Remove old callback-based code

## Alternatives Considered

1. **Keep callback, use `try_send`:** Lossy under pressure, doesn't fix mutex issues.
2. **Unbounded channel:** No backpressure, OOM risk for fast streams.
3. **Single channel to renderer (no fan-out):** Couples session persistence to renderer, harder to test independently.
4. **`std::sync::mpsc`:** Blocks runtime, not suitable for async.

## References

- ADR-0002: Agent emits events to a renderer sink
- Plans: `docs/plans/` (implementation checklists)
