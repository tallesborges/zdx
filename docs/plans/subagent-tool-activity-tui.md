# Subagent Tool Activity in TUI

Show which tools a subagent called — live, while it runs — inside the `invoke_subagent` tool cell in the TUI.

## Goal

When the agent calls `invoke_subagent`, the TUI tool cell shows a live tree of the child tools being called (name + key arg), with spinners for in-progress tools and checkmarks when done. The final `ToolOutput` is still just the subagent's last message text — unchanged.

**Reference**: amp CLI's subagent cell (tree-style indented child tool list):
```
⟳ Invoke_Subagent
  ├── ✓ Read  Cargo.toml
  ├── ✓ Glob  **/main.rs
  ├── ✓ Read  crates/zdx-cli/src/main.rs
  └── ⟳ Bash  cargo build
```

## Non-goals

- Streaming assistant text from the subagent (final message only)
- Bash tool streaming (separate concern)
- New AgentEvent types (reuse `ToolOutputDelta`)
- Exec mode / JSON output changes

## Design

### Event encoding

Reuse `AgentEvent::ToolOutputDelta { id, chunk }` where `id` = the parent
`invoke_subagent` tool call id and `chunk` is a small JSON string encoding child
tool activity:

```json
{"t":"start","id":"tu_abc","name":"Read"}
{"t":"input","id":"tu_abc","arg":"crates/zdx-engine/src/lib.rs"}
{"t":"done","id":"tu_abc"}
{"t":"error","id":"tu_abc"}
```

The TUI parses these only for `invoke_subagent` cells. For any other tool,
`ToolOutputDelta` is ignored (as it is today).

### Key arg extraction

Extracted from `ToolInputCompleted.input` by tool name (same logic as
`tool_display_text` in `cell.rs`):

| Tool | Field |
|---|---|
| `Bash` | `command` (truncated to ~60 chars) |
| `Read` | `file_path` |
| `Glob` | `pattern` |
| `Grep` | `pattern` |
| `Edit` | `file_path` |
| `Write` | `file_path` |
| `Web_Search` | `search_queries[0]` |
| `Fetch_Webpage` | `url` |
| `Invoke_Subagent` | `subagent` or `"task"` |
| others | omit |

## Changes

### 1. `crates/zdx-engine/src/tools/mod.rs`

Add to `ToolContext`:
```rust
pub event_sender: Option<EventSender>,
pub tool_call_id: Option<String>,
```

In `execute_tool()`, clone ctx and set `tool_call_id = Some(tool_use_id)` before
calling the handler. Populate `event_sender` from the agent loop (same place
`ToolContext` is built).

### 2. `crates/zdx-engine/src/tools/subagent.rs`

In `build_exec_options`, widen the filter so the child emits tool lifecycle events:
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

In `execute()`, pass `ctx.event_sender.clone()` and `ctx.tool_call_id.clone()` to
`run_exec_subagent`.

Note: `sanitize_exec_event` in exec mode already passes `ToolStarted`,
`ToolInputCompleted`, and `ToolCompleted` through the `_ => Some(event.clone())`
arm — no changes needed there.

### 3. `crates/zdx-engine/src/core/subagent.rs`

Add optional streaming parameters to `run_exec_subagent`:
```rust
pub struct SubagentStreamSink {
    pub sender: EventSender,
    pub parent_tool_id: String,
}
```

Switch from `wait_with_output()` to line-by-line stdout reading. For each parsed
`AgentEvent` line:

- `ToolStarted { id, name }` →
  emit `ToolOutputDelta { id: parent_tool_id, chunk: json!({ "t": "start", "id": id, "name": name }) }`
- `ToolInputCompleted { id, name, input }` →
  emit `ToolOutputDelta { id: parent_tool_id, chunk: json!({ "t": "input", "id": id, "arg": extract_key_arg(&name, &input) }) }`
- `ToolCompleted { id, result }` →
  emit `ToolOutputDelta { id: parent_tool_id, chunk: json!({ "t": if result.is_ok() { "done" } else { "error" }, "id": id }) }`
- `TurnFinished { final_text, .. }` → capture text, break

All emits use `sender.send_delta()` (best-effort, non-blocking).

### 4. `crates/zdx-tui/src/features/transcript/cell.rs`

Add new types:

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

Add method `apply_child_tool_delta(&mut self, chunk: &str)` that parses the JSON
chunk and:
- `"start"` → push `ChildToolEntry { id, name, state: Running, key_arg: None }`
- `"input"` → find entry by id, set `key_arg`
- `"done"` → find entry by id, set state `Done`
- `"error"` → find entry by id, set state `Error`

On `set_tool_result()` (tool completed), keep `child_tools` — show final list.

**Rendering** — append after the existing tool header line, before the output
preview, only when `child_tools` is non-empty:

```
├── {icon} {name}  {key_arg}
├── {icon} {name}  {key_arg}
└── {icon} {name}  {key_arg}
```

Icons: `⟳` (Running, animated with spinner frame), `✓` (Done, `Style::ToolSuccess`),
`✗` (Error, `Style::ToolError`).

Use `├──` for all but the last entry, `└──` for the last.

### 5. `crates/zdx-tui/src/features/transcript/state.rs`

Add setter mirroring the existing pattern:
```rust
pub fn apply_tool_output_delta_for(&mut self, tool_id: &str, chunk: &str) {
    self.update_cell_by(
        |c| matches!(c, HistoryCell::Tool { tool_use_id, .. } if tool_use_id == tool_id),
        |cell| cell.apply_child_tool_delta(chunk),
    );
}
```

### 6. `crates/zdx-tui/src/features/transcript/update.rs`

Replace the existing TODO:
```rust
// before:
AgentEvent::ToolOutputDelta { .. } => {
    // TODO: Update tool cell with streaming output
    vec![]
}

// after:
AgentEvent::ToolOutputDelta { id, chunk } => {
    transcript.apply_tool_output_delta_for(id, chunk);
    vec![]
}
```

## Delivery order

1. Engine: `ToolContext` fields + `execute_tool` wiring (mod.rs)
2. Engine: widen exec filter + pass sink (subagent.rs)
3. Engine: streaming stdout reader + delta emitter (core/subagent.rs)
4. TUI: `ChildToolEntry` types + `apply_child_tool_delta` + rendering (cell.rs)
5. TUI: state setter (state.rs) + update handler (update.rs)

Each step compiles independently. Steps 1–3 can ship before 4–6 with no visible
change (deltas emitted but ignored by TUI).

## Testing

- Smoke: invoke a subagent in the TUI, verify child tool rows appear live
- Unit: `apply_child_tool_delta` handles start/input/done/error/unknown correctly
- Unit: rendering with 0, 1, N child tools
- Regression: non-subagent tool cells unaffected by `ToolOutputDelta` (no-op)
