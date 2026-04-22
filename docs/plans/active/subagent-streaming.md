# Subagent Streaming

Stream real-time tool activity from child subagent processes to the parent — visible in the TUI as a one-hop child tool activity list inside the `invoke_subagent` cell.

## Goals

- While a subagent runs, the TUI shows a live list of child tool calls (name + key arg, spinner/checkmark)
- Parent emits `ToolOutputDelta` events with structured JSON chunks encoding child tool lifecycle
- Subagent execution switches from blocking `wait_with_output()` to line-by-line stdout streaming with concurrent stderr draining

## Non-goals

- Streaming child assistant text (final message only — unchanged)
- Streaming child bash output (separate concern)
- Nested subagent streaming (grandchild tools are not relayed — exec sanitizer drops `ToolOutputDelta`)
- New `AgentEvent` variants (reuse `ToolOutputDelta`)
- Changes to exec mode CLI output / JSON format
- Full visual parity of child TUI rendering in parent

## Current state (what exists)

### Already done ✅
- `ToolContext` has `event_sender: Option<EventSender>` and `tool_use_id: Option<String>` (`tools/mod.rs:67-70`)
- `bash_handler` already uses these to emit `ToolOutputDelta` for live bash streaming
- TUI `transcript/update.rs` routes `ToolOutputDelta` → `transcript.append_tool_output_delta_for(id, chunk)`
- TUI `cell.rs` has `output_delta: Option<String>` field and `append_tool_output_delta()` method (64KB rolling buffer, 7-line tail preview)
- Child `zdx exec` emits JSONL to stdout; `sanitize_exec_event` passes through `ToolStarted`, `ToolInputCompleted`, `ToolCompleted` (drops deltas + `TurnFinished`)
- `ExecSubagentOptions` supports `event_filter` passed as `--filter` flag

### Not done ❌
- `run_exec_subagent_with_cancel` uses `wait_with_output()` — no streaming at all
- `build_exec_options` sets `event_filter: ["turn_finished"]` — suppresses all tool lifecycle events
- `subagent::execute` does NOT pass `event_sender`/`tool_use_id` to `run_exec_subagent`
- No `ChildToolEntry` types or tree-style rendering in `cell.rs`
- `run_exec_subagent` has no event sink parameter

## Design

### Event encoding

Reuse `AgentEvent::ToolOutputDelta { id, chunk }` where `id` = the parent `invoke_subagent` tool call id and `chunk` is a JSON string encoding child tool activity:

```json
{"t":"start","id":"tu_abc","name":"read"}
{"t":"input","id":"tu_abc","arg":"crates/zdx-engine/src/lib.rs"}
{"t":"done","id":"tu_abc"}
{"t":"error","id":"tu_abc"}
```

Note: tool names are **lowercase** (matching engine normalization in `agent.rs`).

The TUI parses these only for `invoke_subagent` cells. For bash and other tools, `ToolOutputDelta` continues to carry raw text.

### Event ordering

The engine emits events in this order: `ToolInputCompleted` → `ToolStarted` → `ToolCompleted`.
This means `input` (with key arg) arrives *before* `start`. The streaming reader handles this by:
- On `ToolInputCompleted`: emit **both** a synthetic `start` chunk (with name) and an `input` chunk (with key arg) together. This creates the child row immediately with its key arg.
- On `ToolStarted`: if the row already exists (from `ToolInputCompleted`), skip. If no row exists yet (edge case), create it.
- On `ToolCompleted`: update the row state to done/error.

### Key arg extraction

Extracted from `ToolInputCompleted.input` by tool name (lowercase):

| Tool | Field |
|---|---|
| `bash` | `command` (truncated ~60 chars) |
| `read` | `file_path` |
| `glob` | `pattern` |
| `grep` | `pattern` |
| `edit` | `file_path` |
| `write` | `file_path` |
| `web_search` | `search_queries[0]` |
| `fetch_webpage` | `url` |
| `invoke_subagent` | `subagent` or `"task"` |
| others | omit |

### Turn completion semantics

The streaming reader must preserve current failure/interruption handling from `process_subagent_output`:
- `TurnFinished::Completed` → return `final_text`
- `TurnFinished::Interrupted` → return `final_text` (partial is OK)
- `TurnFinished::Failed { message }` → `bail!("Subagent turn failed: {message}")`

## Slices

### Slice 1: Engine — Stream child stdout line-by-line and emit deltas

**Goal**: `run_exec_subagent` reads child stdout incrementally and emits `ToolOutputDelta` events for each tool lifecycle event.

**Changes**:

#### `crates/zdx-engine/src/core/subagent.rs`

Add streaming sink parameter:
```rust
pub struct SubagentStreamSink {
    pub sender: EventSender,
    pub parent_tool_id: String,
}
```

Refactor `run_exec_subagent_with_cancel` to accept `Option<SubagentStreamSink>`:

**Stdout reader** (line-by-line via `BufReader::lines()`):
- For each parsed `AgentEvent` line:
  - `ToolInputCompleted { id, name, input }` → emit synthetic `start` chunk + `input` chunk with `extract_key_arg(&name, &input)`
  - `ToolStarted { id, name }` → emit `start` chunk only if no row exists yet for this id (dedup with `ToolInputCompleted`)
  - `ToolCompleted { id, result }` → emit `done` or `error` chunk (check `result.is_error`)
  - `TurnFinished { status, final_text }` → handle per turn completion semantics above, break
  - Other events → skip

**Stderr reader** (concurrent drain):
- Spawn a separate task to drain child stderr into a `String` buffer via `BufReader::read_to_end()`
- Retained for error diagnostics if the child process fails (current `process_subagent_output` reads stderr on non-zero exit)

**Synchronization**:
- `tokio::select!` on: stdout reader completion, cancellation token, timeout
- After stdout reader finishes (or cancel/timeout), await child process exit
- After child exits, await stderr reader completion
- Do NOT return until both readers are done and the child has exited — prevents pipe deadlocks and lost diagnostics

**Fallback**: If no sink provided, fall back to existing `wait_with_output()` behavior (keeps non-TUI callers working).

Key arg extraction helper:
```rust
fn extract_key_arg(tool_name: &str, input: &Value) -> Option<String> { ... }
```

#### `crates/zdx-engine/src/tools/subagent.rs`

1. Widen event filter in `build_exec_options`:
```rust
// before:
event_filter: Some(vec!["turn_finished".to_string()])
// after:
event_filter: Some(vec![
    "turn_finished".to_string(),
    "tool_started".to_string(),
    "tool_input_completed".to_string(),
    "tool_completed".to_string(),
])
```

2. In `execute()`, build a `SubagentStreamSink` from `ctx.event_sender` + `ctx.tool_use_id` and pass it to `run_exec_subagent_with_cancel`.

**Demo**: Invoke a subagent in the TUI → see raw JSON chunks appear in the tool output delta area (unformatted, but live).

### Slice 2: TUI — Child tool activity list rendering

**Goal**: Parse the JSON chunks from Slice 1 into a structured list and render it inside the `invoke_subagent` tool cell.

**Changes**:

#### `crates/zdx-tui/src/features/transcript/cell.rs`

Add types:
```rust
pub struct ChildToolEntry {
    pub id: String,
    pub name: String,
    pub key_arg: Option<String>,
    pub state: ChildToolState,
}

pub enum ChildToolState {
    Running,
    Done,
    Error,
}
```

Add field to `HistoryCell::Tool`:
```rust
child_tools: Vec<ChildToolEntry>,
```

Add method `apply_child_tool_delta(&mut self, chunk: &str)`:
- Try parse as JSON `{"t":...}`
- `"start"` → if no entry with this id exists, push `ChildToolEntry { id, name, state: Running, key_arg: None }`; otherwise skip (dedup — `ToolInputCompleted` may have created it first)
- `"input"` → find entry by id, set `key_arg`; if no entry exists, create one with `Running` state and the key arg
- `"done"` → find entry by id, set state `Done`
- `"error"` → find entry by id, set state `Error`
- Non-JSON or unknown → fall through to existing `append_tool_output_delta` (raw text append)

**Detection**: Only try JSON parsing when the tool name is `invoke_subagent`. For all other tools, use raw text append as today.

**Persistence**: `child_tools` is preserved when `set_tool_result()` is called (tool completed). The completed cell shows the final child tool list alongside the result.

Rendering — append after the tool header, before output preview:
```
├── ✓ read  Cargo.toml
├── ✓ glob  **/main.rs
└── ⟳ bash  cargo build
```

Icons: `⟳` (Running, animated with spinner frame), `✓` (Done), `✗` (Error).
Use `├──` for all but last, `└──` for last.

#### `crates/zdx-tui/src/features/transcript/state.rs`

Ensure `append_tool_output_delta_for` routes to `apply_child_tool_delta` for subagent cells (or the cell method itself handles detection internally).

Note: currently completed cells may drop late `ToolOutputDelta` events. Verify this doesn't cause the last few child tool state updates to be lost — the stdout reader should finish emitting all deltas before the parent `ToolCompleted` event fires.

#### `crates/zdx-tui/src/overlays/tool_detail.rs`

When viewing a completed `invoke_subagent` tool in the overlay, render the full child tool list above the final output text.

**Demo**: Invoke a subagent in the TUI → see child tools appear live with spinners, then checkmarks when done.

## Delivery order

1. Slice 1 (engine) — compiles independently, emits deltas the TUI shows as raw text
2. Slice 2 (TUI) — parses deltas into structured list rendering

Each slice compiles independently. Slice 1 can ship before Slice 2 — deltas are emitted but TUI shows them as raw text in the output_delta preview (live-only, cleared on tool completion).

## Files touched

| File | Slice | Change |
|---|---|---|
| `crates/zdx-engine/src/core/subagent.rs` | 1 | Streaming reader, stderr drain, `SubagentStreamSink`, `extract_key_arg` |
| `crates/zdx-engine/src/tools/subagent.rs` | 1 | Widen event filter, pass sink from `execute()` |
| `crates/zdx-tui/src/features/transcript/cell.rs` | 2 | `ChildToolEntry` types, `apply_child_tool_delta`, list rendering |
| `crates/zdx-tui/src/features/transcript/state.rs` | 2 | Verify delta routing for subagent cells |
| `crates/zdx-tui/src/overlays/tool_detail.rs` | 2 | Render child tool list in overlay |

## Testing

- Smoke: invoke a subagent in the TUI, verify child tool rows appear live
- Unit: `extract_key_arg` for each tool name (lowercase)
- Unit: `apply_child_tool_delta` handles start/input/done/error/unknown, including out-of-order (input before start)
- Unit: rendering with 0, 1, N child tools
- Unit: streamed parsing handles `TurnFinished::Completed`, `Interrupted`, and `Failed` correctly
- Regression: non-subagent tool cells unaffected (bash output_delta still works)
- Regression: `process_subagent_output` still works when no sink provided (non-streaming fallback)

## Risks

- **Pipe deadlock**: If stderr is not drained concurrently, child can block on full stderr pipe → mitigated by dedicated stderr drain task
- **Event ordering**: `ToolInputCompleted` before `ToolStarted` → mitigated by synthetic `start` from `ToolInputCompleted`
- **Late deltas dropped**: Completed cells may ignore late `ToolOutputDelta` → mitigated by ensuring stdout reader finishes all emissions before parent `ToolCompleted` fires
- **Large child tool inputs**: `ToolInputCompleted` for `write`/`edit` can carry large input JSON just to extract a small key arg → `extract_key_arg` reads only the needed field, does not clone the full input
- **Child stdout buffering**: May delay lines → mitigated by `BufReader::lines()` on async reader
- **Nested subagents**: Grandchild tool activity is NOT visible (exec sanitizer drops `ToolOutputDelta`). Nested `invoke_subagent` shows as a single row. True nested streaming is a separate future effort.
- **Large number of child tools**: Could bloat cell height → cap display to last ~20 entries with a "N more..." line
- **Coupling**: Depends on current exec JSONL contract. If `json-default-output.md` changes exec event names/payloads, this plan must be updated in lockstep.
