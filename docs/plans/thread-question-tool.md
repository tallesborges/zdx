# Goals
- Provide a tool that accepts a thread ID and a goal, and returns a single response grounded in that thread.
- Ensure the tool output is response-only (no extra metadata or formatting).
- Reuse existing thread retrieval and model execution paths where possible.

# Non-goals
- UI changes or new navigation flows.
- Writing or mutating thread logs.
- Multi-turn follow-ups within a single tool call.

# Design principles
- User journey drives order
- Inputs are explicit: thread ID + goal
- Output is response-only

# User journey
1. User identifies the target thread ID.
2. User calls the tool with the thread ID and a goal.
3. User receives a single response derived from that thread context.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Thread persistence and retrieval
- What exists: Threads are stored and can be listed/shown/resumed by ID.
- ✅ Demo: List threads and show a specific thread by ID.
- Gaps: None for basic retrieval.

## Handoff-style subagent execution
- What exists: A subagent path that builds a prompt from thread context and returns a response-only output.
- ✅ Demo: Trigger a handoff and receive the generated prompt text only.
- Gaps: Not parameterized for arbitrary goals.

## Tool system with deterministic results
- What exists: Tools accept structured inputs and return deterministic envelopes.
- ✅ Demo: Call an existing tool and observe structured success/error results.
- Gaps: No read_thread tool yet.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: Read-thread core (happy path)
- **Goal**: Answer a goal against a specified thread ID using existing model execution flow.
- **Scope checklist**:
  - [ ] Load thread content by ID for context.
  - [ ] Build a prompt that uses thread context + goal.
  - [ ] Execute the model call and capture the response text only.
  - [ ] Return a response-only result for the happy path.
- **✅ Demo**: Invoke the tool with a valid thread ID and goal; receive a response-only output.
- **Risks / failure modes**:
  - Thread content too large or malformed.
  - Response includes extra text outside the expected response-only format.

## Slice 2: Tool contract + error handling
- **Goal**: Expose the feature as a tool with strict input/output contracts and robust errors.
- **Scope checklist**:
  - [ ] Define tool schema (thread_id, goal).
  - [ ] Enforce response-only output contract.
  - [ ] Handle invalid thread IDs and empty goals with clear errors.
  - [ ] Ensure tool is read-only (no thread mutation).
- **✅ Demo**: Invalid thread ID returns a clean error; valid call returns response-only output.
- **Risks / failure modes**:
  - Ambiguous error messages.
  - Tool output drifting from response-only requirement.

## Slice 3: Thread picker trigger (`@@`)
- **Goal**: Reuse the existing thread picker overlay to insert a thread ID.
- **Scope checklist**:
  - [ ] Add `@@` trigger to open the thread picker overlay.
  - [ ] In insert mode, selecting a thread inserts the thread ID into input.
  - [ ] Keep the thread picker filter input for narrowing results.
- **✅ Demo**: Type `@@`, pick a thread, and the selected thread ID is inserted.
- **Risks / failure modes**:
  - Overlay conflicts with existing input modes.
  - Selected thread ID is not inserted correctly.

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- Tool output is response-only.
- Tool inputs are strictly thread ID + goal.
- Tool does not mutate thread logs.
- Invalid thread ID or empty goal yields a clear error.

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- Which model/config to use for read_thread execution (default vs handoff model).
- Prompt framing to ensure response-only output.
- How much thread content to include if size limits are hit.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: Response-only prompt refinement
- Tighten prompt to reduce extra formatting or meta text.
- ✅ Check-in demo: Multiple goals return clean response-only outputs.

# Later / Deferred
Explicit list of "not now" items + what would trigger revisiting them.
- None beyond the tool described; revisit if requirements expand.