# Streaming Tool Output — Shipped ✅

## Status: Complete (all 3 slices shipped)

**Files changed** (9 files, +323 / -70):
- `crates/zdx-tui/src/features/transcript/cell.rs` — `output_delta` field, append method, display, clear logic
- `crates/zdx-tui/src/features/transcript/state.rs` — `append_tool_output_delta_for` with Running guard
- `crates/zdx-tui/src/features/transcript/update.rs` — wired `ToolOutputDelta` event
- `crates/zdx-tui/src/overlays/tool_detail.rs` — popup output priority + preserved partial output
- `crates/zdx-engine/src/tools/mod.rs` — `event_sender`/`tool_use_id` on ToolContext, bash_handler channel bridge
- `crates/zdx-engine/src/core/agent.rs` — set event_sender/tool_use_id per tool spawn
- `crates/zdx-tools/src/bash.rs` — line-buffered streaming reads, timeout/guard fixes
- `crates/zdx-tools/src/lib.rs` — pre-existing clippy fix
- `crates/zdx-tui/src/runtime/handlers/bash.rs` — updated bash::run() caller

# Goals
- Show real-time stdout/stderr from running tools (especially `bash`) in the TUI — both in the compact transcript cell and in the tool detail popup
- Wire up the existing `ToolOutputDelta` event so incremental output flows from tool execution → engine event channel → transcript state → rendered UI

# Non-goals
- Changing the final `ToolCompleted` flow (it continues to set the authoritative result)
- Streaming output for non-bash tools (read, grep, glob, etc. complete near-instantly)
- Real PTY/terminal emulation (ANSI escape handling, interactive input)
- Streaming output for subagent tools (separate concern)
- `read_buf`-based chunking or UTF-8 boundary handling (uses `read_line`)

# Design decisions (as shipped)
- **Sender threading: Option B** — `Option<mpsc::UnboundedSender<String>>` passed to bash by value. `zdx-tools` stays free of engine dependencies. Engine `bash_handler` creates channel, uses `tokio::join!` to run bash + forwarding concurrently — guarantees no `ToolOutputDelta` after `ToolCompleted`.
- **Chunk granularity: `read_line`** — line-buffered via `tokio::io::BufReader` + `AsyncBufReadExt::read_line`. Lower event volume, no UTF-8 boundary bugs.
- **Stderr handling: interleaved** — both stdout and stderr lines go to `output_delta`. Final `ToolOutput` still separates them.
- **Tail buffer cap: 64KB** — `output_delta` keeps only the last 64KB (char-boundary-safe truncation from front). Bounds memory for long-running commands.
- **Clear on Done, preserve on cancel/error** — `set_tool_result` clears `output_delta` only on `ToolState::Done`. Cancelled/errored tools keep partial output visible.
- **Spawned reader tasks** — stdout/stderr readers use `tokio::spawn` (not inline futures) so timeout cancellation doesn't drop in-progress `read_line` calls. Tasks drain to EOF after process kill, then are joined.

# Architecture

```
bash process → stdout/stderr pipes
    ↓ (line-buffered via BufReader::read_line)
tokio::spawn reader tasks → mpsc::UnboundedSender<String>
    ↓ (channel bridge in bash_handler via tokio::join!)
EventSender → AgentEvent::ToolOutputDelta { id, chunk }
    ↓ (TUI event loop)
TranscriptState::append_tool_output_delta_for(id, chunk)
    ↓ (render-time cell lookup)
Compact cell: tail_rendered_rows (max 7 rows)
Tool detail popup: full output with auto-scroll
```

# Shipped slices

## Slice 1: TUI consumer ✅

- [x] `output_delta: Option<String>` field on `HistoryCell::Tool`
- [x] `append_tool_output_delta()` with 64KB tail cap (char-boundary-safe)
- [x] `append_tool_output_delta_for()` on `TranscriptState` with `ToolState::Running` guard
- [x] `ToolOutputDelta` event wired in `update.rs`
- [x] Compact cell shows `output_delta` tail preview (7 rows, `Style::ToolOutput`)
- [x] `set_tool_result` clears `output_delta` only on `Done`
- [x] Popup priority: `output_delta` (white) → `input_delta` (cyan) → "Waiting for output…"
- [x] Popup shows preserved partial output for cancelled/errored tools (DarkGray)
- [x] Empty `input_delta` filtered (doesn't suppress "Waiting…" placeholder)

## Slice 2: Producer plumbing ✅

- [x] `output_tx: Option<UnboundedSender<String>>` param on `bash::execute()`, `run()`, `run_command()`
- [x] `event_sender: Option<EventSender>` and `tool_use_id: Option<String>` on engine `ToolContext`
- [x] `bash_handler` creates channel bridge with `tokio::join!` (structured task, no detached bridge)
- [x] `execute_tools_async` sets `event_sender`/`tool_use_id` on ctx per tool spawn
- [x] All callers updated (`bash_handler`, TUI `bash.rs` handler)
- [x] Timeout path sends timeout message via `output_tx`

## Slice 3: Streaming I/O ✅

- [x] Replaced `child.wait_with_output()` with `tokio::spawn` reader tasks
- [x] Line-buffered reads via `BufReader::read_line` on both stdout and stderr
- [x] Each line sent via `output_tx` as it arrives
- [x] Output accumulated in `Vec<u8>` buffers for final `BashOutput`
- [x] Timeout: wraps only `child.wait()`, kills process group/child, reaps, joins readers, preserves partial output
- [x] `ProcessGroupGuard` disarmed on all paths (normal, timeout, error)
- [x] Non-Unix: explicit `child.kill()` on timeout
- [x] Truncation (40KB per stream) + temp files still applied to final output
- [x] Timeout `BashOutput` includes partial output captured before timeout

# Contracts (enforced)
- `ToolCompleted` is the authoritative source of truth for final tool output
- **No `ToolOutputDelta(id)` after `ToolCompleted(id)`** — enforced by `tokio::join!` in bash_handler
- **TUI ignores deltas for non-Running tools** — guard in `append_tool_output_delta_for`
- `output_delta` is transient — not persisted, not in message history
- 40KB per stream truncation preserved in final `ToolOutput`
- Process group cleanup works on all paths (normal, timeout, cancel)
- Non-bash tools unaffected (event_sender/tool_use_id ignored)

# Polish phases (not yet shipped)

## Phase 1: Output delta coalescing
- Coalesce rapid `ToolOutputDelta` events to reduce render pressure
- ✅ Check-in demo: `yes | head -1000` → smooth scrolling, no frame drops

## Phase 2: Stderr differentiation in popup
- Show stderr in a different color in the popup
- Requires changing `output_delta` to a structured type with stream tags
- ✅ Check-in demo: mixed stdout+stderr → visually distinct in popup

# Later / Deferred
- **`read_buf` chunking**: For non-line-oriented output. Trigger: user reports of delayed progress.
- **Streaming for subagent tools**: Subagent → parent event forwarding. Trigger: subagent UX maturity.
- **ANSI escape handling**: Strip or interpret ANSI codes. Trigger: user complaints (unlikely given `TERM=dumb` + `NO_COLOR=1`).
- **Interactive bash / PTY**: Full terminal emulation. Trigger: clear user demand.
- **Non-bash tool streaming**: `fetch_webpage`, `web_search` progress. Trigger: user feedback.
