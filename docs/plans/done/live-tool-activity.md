# Live tool activity in the run marker

> **Status: DONE (2026-08-26).** Implemented as planned. Option chosen from
> exploration: extend the `agent_activity` run marker with current-tool state
> instead of persisting in-flight events to thread JSONL or adding an SSE
> layer to the bot server.

## Problem

Monitor and the Mini App only see persisted thread state. The engine commits
`tool_use`/`tool_result` to `$ZDX_HOME/threads/<id>.jsonl` at `TurnCheckpoint`,
**after** a tool round completes (`thread_persistence/persist.rs`,
`replay.rs`). So while a long `bash` command runs, disk readers show nothing —
you can't tell *what* is currently executing. Only the TUI (and the bot status
message) see live `AgentEvent`s.

Meanwhile `agent_activity.rs` already maintains a cross-process active-run
registry: an RAII `RunGuard` writes an atomic JSON marker to
`$ZDX_HOME/run/agents/<pid>-<uuid>.json` for every `run_turn_inner` and
removes it on drop. Monitor (`load_active_agents`, 1s tick), the Mini App
(`GET /api/monitor` → `MonitorAgent`), the TUI thread picker, and
`zdx service restart` all read it. It just carries run metadata, not tool
state.

## Goal

Every surface that reads the run registry can show *what each active run is
executing right now* — tool name + input summary (for `bash`, the actual
command), for main runs and subagents alike.

## Non-goals

- Live tool **output** streaming to monitor/Mini App (that would be an SSE
  layer in the bot server — separate, later).
- Persisting in-flight events to thread JSONL (fights the checkpoint
  persistence model; unmatched `ToolUse` renders as cancelled on replay).
- Changing the Mini App's 30s monitor poll cadence (tunable later).

## Design

### 1. Marker schema (additive, no version bump)

`RunRecord` gains:

```rust
#[serde(default)]
pub current_tools: Vec<ActiveToolCall>,

pub struct ActiveToolCall {
    pub id: String,        // tool_use id
    pub name: String,      // e.g. "bash"
    pub summary: String,   // e.g. "cargo build --release" (truncated ~200 chars)
    pub started_at: String // RFC 3339
}
```

`Vec` because tool rounds execute in parallel (`JoinSet`). Old markers
deserialize fine via `serde(default)`.

### 2. RunGuard becomes updatable

- `RunGuard { path, record: Mutex<RunRecord> }`.
- New methods, same tempfile+persist atomic-write pattern as `start()`:
  - `set_tools_started(&self, tools: &[ActiveToolCall])` — replace the list
    for the new round.
  - `set_tool_finished(&self, id: &str)` — remove one entry.
- Best-effort like `start()`: failures are ignored, never break the turn.
- Write cost: one small atomic write per round start + one per tool
  completion. Nothing on hot streaming paths.

### 3. Engine hook points (`core/agent.rs`)

`run_turn_inner` already owns `_run_guard`. Thread `Option<&RunGuard>` down
through `process_tool_turn` → `execute_tools_async`:

- In `emit_tool_started_events` (called with the round's full `&[ToolUse]`,
  inputs included): build summaries and call `set_tools_started`.
- In `record_tool_completion`: call `set_tool_finished(id)`. This covers both
  the spawned-task path and the synchronous `todo_write` path.
- In `handle_tool_interrupt`: clear the list (`set_tools_started(&[])`).

Subagents run through the same `run_turn_inner`, so their markers (already
grouped under `parent_thread_id` in monitor's tree) get tool state for free.

### 4. Input summary helper

`zdx_transcript::cell::tool_command_text(name, &Value) -> String` already
produces exactly the right one-line summary (bash → command, edit/read/write
→ path, etc.), but `zdx-transcript` depends on `zdx-engine`, so the engine
can't call it. Move it (pure function) to `zdx-types` (charter: pure helper
logic for tools), have `zdx-transcript` call the moved fn, and use it from
`agent_activity` with truncation to ~200 chars. No duplicate logic.

### 5. Consumers

- **Monitor** (`crates/zdx-monitor/src/app.rs` `load_active_agents` →
  `ActiveAgentInfo`): add `current_tool: Option<String>` (render the first
  entry, `+N` suffix when parallel). Show in the agents table row and/or the
  selected-agent detail. The existing 1s `refresh_app` tick picks it up —
  no cadence change.
- **Mini App** (`crates/zdx-bot/src/server.rs` `MonitorAgent` + `app.html`
  monitor page): add the same field, render under each active agent.
- **TUI / service restart**: no changes; they ignore the new field.

## Commit-sized steps

1. **types**: move `tool_command_text` into `zdx-types`; `zdx-transcript`
   delegates to it. (`just ci-fast`, transcript tests still pass.)
2. **engine**: `ActiveToolCall` + `RunRecord.current_tools` + `RunGuard`
   update methods + hooks in `execute_tools_async` /
   `record_tool_completion` / `handle_tool_interrupt`. Unit test in
   `agent_activity`: start → set tools → `list_active` shows them → finish →
   empty.
3. **monitor**: surface `current_tool` in the agents view.
4. **bot server + app.html**: expose in `/api/monitor`, render on the
   monitor page.

## Verification

- Unit: `cargo nextest run -p zdx-engine agent_activity`.
- Manual: in one terminal ask the TUI to run `bash sleep 30`; in another,
  `cat $ZDX_HOME/run/agents/*.json` shows the command; `just monitor` shows
  it in the agents view within ~1s; Mini App monitor page shows it after
  refresh. On completion/interrupt the entry disappears; on process kill the
  stale-PID cleanup already removes the marker.
- `just ci-fast` per step; `just test` before moving the plan to `active/`.

## Risks

- **Marker churn**: rewrites on every tool start/finish; tiny JSON, atomic
  rename, same dir — negligible.
- **Sensitive input in summaries**: bash commands may contain secrets; the
  marker is a local file under `$ZDX_HOME` with the same exposure as thread
  JSONL, which already stores full inputs. Accepted.
- **Guard threading**: `execute_tools_async` gains one parameter; keep it
  `Option<&RunGuard>` so tests/callers without a guard pass `None`.
