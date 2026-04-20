# Goals
- Subagent `invoke_subagent` calls stream stderr output from the child process (tool progress, thinking) to the parent
- Exec mode can output JSON events for machine consumption
- Exec mode provides richer output with filterable detail levels

# Non-goals
- Full visual parity between parent and child TUI rendering for subagent output
- Breaking existing exec mode text output (must be backward compatible)
- Deep refactoring of the event system beyond enabling subagent streaming

# Design principles
- User journey drives order
- Backward compatibility for default text output
- Opt-in for new features (JSON output, detail levels)
- Minimal, isolated changes per slice
- Leverage existing `ToolOutputDelta` event type

# User journey
1. User executes a command that invokes a subagent
2. During subagent execution, real-time progress (tool status, thinking) streams into the parent's stderr
3. Upon subagent completion, the final result is displayed seamlessly
4. (Optional) User runs `zdx exec --json` and receives structured JSONL events
5. (Optional) User runs `zdx exec --detail debug` and sees richer output

# Foundations / Already shipped (✅)

## Child process spawning
- What exists: `run_exec_subagent` spawns `zdx exec` with `Stdio::piped()` for stdout/stderr
- ✅ Demo: `invoke_subagent` works end-to-end, returns final text
- Gaps: Uses `wait_with_output()` — no streaming

## Event system
- What exists: `AgentEvent::ToolOutputDelta { id, chunk }` defined, event channels with broadcaster
- ✅ Demo: Events flow from agent → broadcaster → renderer + persistence tasks
- Gaps: `ToolOutputDelta` never emitted by subagent, ignored by exec renderer

## Exec renderer
- What exists: `ExecRenderer` handles stdout/stderr separation, renders `ToolStarted`/`ToolCompleted`
- ✅ Demo: `zdx exec` streams assistant text to stdout, tool status to stderr
- Gaps: `ToolOutputDelta`, `ToolInputDelta`, `UsageUpdate` are no-ops

# MVP slices (ship-shaped, demoable)

## Slice 1: Stream subagent stderr to parent as `ToolOutputDelta`
- **Goal**: When `invoke_subagent` runs, parent sees real-time stderr from the child process
- **Scope checklist**:
  - [ ] Refactor `run_exec_subagent` to accept an optional `AgentEventTx` sender
  - [ ] Instead of `wait_with_output()`, take child stderr pipe and spawn a tokio task to read lines
  - [ ] For each stderr line, emit `ToolOutputDelta { id: tool_use_id, chunk: line }`
  - [ ] Still collect stdout for the final return value
  - [ ] Thread the tool_use_id from `subagent::execute` through to `run_exec_subagent`
  - [ ] Update `ExecRenderer::handle_event` to render `ToolOutputDelta` to stderr (prefixed with `  ` indent)
- **✅ Demo**: Run `zdx exec` with a prompt that triggers `invoke_subagent`. See child's tool status and thinking stream in real-time on stderr
- **Risks / failure modes**:
  - Child stderr buffering may cause delayed output — mitigate with `BufReader::lines()`
  - Need to handle child process exit cleanly even if stderr reader task is still running

## Slice 2: JSON output mode for exec
- **Goal**: `zdx exec --format json` outputs structured JSONL events to stdout
- **Scope checklist**:
  - [ ] Add `--format` flag to exec command (`text` default, `json` option)
  - [ ] Create `JsonExecRenderer` that serializes each `AgentEvent` as one JSON line to stdout
  - [ ] Pass format option through `ExecRunOptions` → `ExecOptions` → renderer selection
  - [ ] In JSON mode, ALL events are emitted (no filtering)
- **✅ Demo**: Run `zdx exec --format json -p "hello"` and pipe through `jq`. Verify valid JSONL with event types
- **Risks / failure modes**:
  - Must not mix JSON stdout with plain text stderr — JSON renderer writes only to stdout
  - Thread persistence must still work in JSON mode

## Slice 3: Richer exec text output with detail levels
- **Goal**: `zdx exec --verbose` shows more detail (tool input/output deltas, usage stats)
- **Scope checklist**:
  - [ ] Add `--verbose` / `-v` flag to exec command
  - [ ] In verbose mode, render `ToolInputDelta` (show streaming tool input on stderr)
  - [ ] In verbose mode, render `UsageUpdate` (show token counts on stderr after turn)
  - [ ] In verbose mode, show `ToolOutputDelta` with more context (tool name prefix)
- **✅ Demo**: Run `zdx exec -v -p "list files"` and see tool input streaming + usage stats on stderr
- **Risks / failure modes**:
  - Verbose output could be noisy — keep formatting clean with clear prefixes

# Contracts (guardrails)
- Default `zdx exec` text output must remain identical (backward compatible)
- `ToolOutputDelta` uses the parent's `tool_use_id` for the `invoke_subagent` call
- JSON output is JSONL (one JSON object per line) to stdout only
- `--format json` and `--verbose` are independent (JSON always includes all events)

# Key decisions (decide early)
- **How to pass event sender to subagent tool**: Add `AgentEventTx` to `ToolContext` so `subagent::execute` can forward it to `run_exec_subagent`
- **Stream stderr only vs both**: Start with stderr only (contains tool status + thinking); stdout is the final response text
- **`--format` vs `--json` flag**: Use `--format json` for extensibility (could add `--format markdown` later)

# Testing
- Manual smoke demos per slice
- Integration test: verify default exec output unchanged (regression)
- Integration test: verify `--format json` produces valid JSONL
- Unit test: `ExecRenderer` handles `ToolOutputDelta` in verbose mode

# Polish phases (after MVP)

## Phase 1: TUI subagent streaming
- Render `ToolOutputDelta` in TUI tool output panel for subagent calls
- Show last N lines of subagent output (scrolling window)
- ✅ Check-in demo: Invoke subagent in TUI, see streaming output in tool panel

## Phase 2: Exec output refinements
- Add `--quiet` / `-q` flag (suppress all stderr except errors)
- Color-code stderr output (tool names, durations, errors)
- ✅ Check-in demo: Compare `-q`, default, and `-v` output side by side

# Later / Deferred
- Streaming child stdout (assistant text) as separate event type — revisit if needed for orchestrator patterns
- Full child event relaying (re-parsing child JSON output to get structured events) — complex, defer until JSON mode is stable
- `--filter` flag for selective event type output — revisit after JSON mode is used in practice
