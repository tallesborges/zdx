# Plan: Parallel Tool Execution

## Goals
- Execute multiple tool_use requests in parallel instead of sequentially
- Reduce latency when Claude requests multiple independent tools (e.g., 4 file reads)
- Maintain correct event ordering for UI (ToolStarted/ToolFinished per tool)
- Preserve interrupt handling behavior (cancel all on Ctrl+C)

## Non-goals
- Changing the tool execution timeout mechanism
- Adding concurrency limits or throttling (defer until needed)
- Reordering tool results (must match tool_uses order for API)
- Changing the ToolOutput/ToolResult data structures
- Killing `spawn_blocking` tasks on interrupt (not possible with tokio)

## Design principles
- **User journey drives order**: parallel execution is the entire feature—ship it first
- **Preserve contracts**: event emission order per-tool must stay ToolStarted→ToolFinished
- **Centralized event emission**: tasks return data, coordinator emits events (avoids races/duplicates)
- **Minimal change**: modify only `execute_tools_async`; don't restructure surrounding code

## User journey
1. User sends a prompt that triggers Claude to request multiple tools (e.g., read 4 files)
2. Agent receives 4 tool_use blocks in a single response
3. All 4 tools execute concurrently (not sequentially)
4. User sees faster completion (wall-clock time ≈ slowest tool, not sum of all)
5. If user presses Ctrl+C, all running tools are cancelled (async portion; blocking work may complete)

## Foundations / Already shipped (✅)

| What exists | ✅ Demo | Gaps |
|-------------|---------|------|
| `execute_tools_async` function | Run agent, observe sequential tool execution in TUI | Runs sequentially—this is what we're fixing |
| `tools::execute_tool` is already async | Check `src/tools/mod.rs:172` | None |
| `EventSink` for emitting events | Events appear in TUI during tool execution | None |
| Interrupt handling via `tokio::select!` | Press Ctrl+C during tool execution | Need to adapt for concurrent tasks |
| `ToolResult` ordering matches `tool_uses` | Check message history after multi-tool turn | Must preserve in parallel impl |

## MVP Slice: Parallel tool execution with `tokio::JoinSet`

### Goal
All tool_uses execute concurrently; results collected in original order.

### Scope checklist
- [x] Replace sequential `for` loop with `tokio::JoinSet` spawning
- [x] **Keep ToolStarted emission sequential** (emit in loop before spawning each task to preserve CLI output)
- [x] Spawn each tool execution returning `(index, ToolOutput, ToolResult)` tuple
- [x] Clone `ToolUse` and `ToolContext` into each task (JoinSet requires `'static`)
- [x] Use `Vec<Option<(ToolOutput, ToolResult)>>` with pre-allocated slots for ordering
- [x] Collect results via `join_set.join_next()` loop, filling slots by index
- [x] **Centralize ToolFinished emission**: emit from coordinator as each task completes (not inside tasks)
- [x] Handle interrupt: `join_set.abort_all()`, track which tools completed, emit abort for incomplete only
- [x] Handle `JoinError` (panic/cancellation) → map to `ToolOutput::failure("panic", ...)`

### ✅ Demo
1. Prompt: "Read the contents of Cargo.toml, src/main.rs, src/config.rs, and README.md"
2. Observe: 4 `ToolStarted` events appear sequentially (clean CLI output)
3. Observe: Total time ≈ slowest read, not sum of 4 reads
4. Press Ctrl+C during execution → incomplete tools show "Interrupted", no duplicate events

### Risks / failure modes
- `spawn_blocking` tasks continue after abort (accept: file reads are fast, writes are atomic)
- Event ordering if `join_next()` returns out-of-order (mitigate: ToolFinished emitted per-completion is fine; results vector preserves order)
- Race between interrupt check and task completion (mitigate: track completed set, don't double-emit)

## Contracts (guardrails)
1. **Event sequence per tool**: `ToolStarted{id}` always precedes `ToolFinished{id}` for same tool
2. **Result order**: `Vec<ToolResult>` returned must match `tool_uses` input order
3. **Interrupt behavior**: Incomplete tools get abort results; completed tools keep their results
4. **No duplicate events**: Each tool gets exactly one `ToolFinished` (success, error, or abort)
5. **No data loss**: Every tool_use gets exactly one ToolResult in returned vec

## Key decisions
1. **Use `JoinSet`**: Provides `abort_all()` and `join_next()` for clean interrupt handling
2. **ToolStarted timing**: Emit sequentially before spawning (preserves CLI output format)
3. **ToolFinished timing**: Emit from coordinator as tasks complete (avoids races)
4. **Result ordering**: `Vec<Option<_>>` with index tracking, convert to `Vec<_>` at end
5. **Interrupt dedup**: Track `HashSet<usize>` of completed indices; only abort incomplete

## Implementation sketch

```rust
async fn execute_tools_async(
    tool_uses: &[ToolUse],
    ctx: &ToolContext,
    sink: &EventSink,
) -> Vec<ToolResult> {
    use std::collections::HashSet;
    use tokio::task::JoinSet;

    let mut join_set: JoinSet<(usize, String, ToolOutput, ToolResult)> = JoinSet::new();
    let mut results: Vec<Option<(ToolOutput, ToolResult)>> = vec![None; tool_uses.len()];
    let mut completed: HashSet<usize> = HashSet::new();

    // Emit ToolStarted sequentially, then spawn tasks
    for (i, tu) in tool_uses.iter().enumerate() {
        sink.important(AgentEvent::ToolStarted {
            id: tu.id.clone(),
            name: tu.name.clone(),
        })
        .await;

        // Clone for 'static requirement
        let tu = tu.clone();
        let ctx = ctx.clone();

        join_set.spawn(async move {
            let (output, result) = tools::execute_tool(&tu.name, &tu.id, &tu.input, &ctx).await;
            (i, tu.id.clone(), output, result)
        });
    }

    // Collect results with interrupt handling
    loop {
        tokio::select! {
            biased;
            _ = crate::core::interrupt::wait_for_interrupt() => {
                // Abort all remaining tasks
                join_set.abort_all();

                // Emit abort for incomplete tools
                for (i, tu) in tool_uses.iter().enumerate() {
                    if !completed.contains(&i) {
                        let abort_output = ToolOutput::canceled("Interrupted by user");
                        sink.important(AgentEvent::ToolFinished {
                            id: tu.id.clone(),
                            result: abort_output.clone(),
                        })
                        .await;
                        results[i] = Some((abort_output.clone(), ToolResult::from_output(tu.id.clone(), &abort_output)));
                    }
                }
                break;
            }
            task_result = join_set.join_next() => {
                match task_result {
                    Some(Ok((idx, id, output, result))) => {
                        completed.insert(idx);
                        sink.important(AgentEvent::ToolFinished {
                            id,
                            result: output.clone(),
                        })
                        .await;
                        results[idx] = Some((output, result));
                    }
                    Some(Err(e)) => {
                        // JoinError: panic or cancellation
                        // Find which task failed (tricky—may need to track differently)
                        // For now, this is rare; log and continue
                        eprintln!("Task join error: {:?}", e);
                    }
                    None => break, // All tasks completed
                }
            }
        }
    }

    // Convert to Vec<ToolResult>, unwrapping Options
    results
        .into_iter()
        .map(|opt| opt.expect("all slots should be filled").1)
        .collect()
}
```

## Testing
- **Manual smoke demo**: Multi-file read prompt, verify parallel execution via timing
- **Interrupt test**: Ctrl+C during 4-tool execution, verify incomplete show "Interrupted", no duplicates
- **Existing test**: Update `test_execute_tools_emits_events` for new event timing

## Polish phases (after MVP)

### Phase 1: Concurrent event emission refinement
- ✅ Check-in: Events for parallel tools render cleanly in TUI without visual glitches
- Scope: Adjust TUI rendering if ToolFinished events arriving out-of-order cause issues

## Later / Deferred

| Item | Trigger to revisit |
|------|-------------------|
| Concurrency limit (max N parallel tools) | If tool execution causes resource exhaustion |
| Determinism guard for conflicting tools | If parallel edit/write to same file causes issues |
| Cancellation token for `spawn_blocking` | If background work after interrupt is problematic |
| Per-tool timeout refinement | If parallel execution changes timeout semantics |
