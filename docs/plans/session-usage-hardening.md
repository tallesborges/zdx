# Session Usage Hardening Plan

## Inputs

- **Project/feature**: Harden session usage tracking to handle edge cases: duplicate UsageUpdate events, backward compatibility with old session files, and improve code clarity.
- **Existing state**: Per-request delta storage implemented. Usage saved on `output_tokens > 0` and on turn completion/interruption. `extract_usage_from_events()` sums all Usage events for cumulative, takes last for latest.
- **Constraints**: Must not break existing sessions. No schema migration required for users. Changes should be backward compatible.
- **Success looks like**: Usage is never double-counted, old sessions load correctly, code is clear about "request" vs "turn" semantics.

---

# Goals

- Prevent duplicate saves from multiple `UsageUpdate` events with `output_tokens > 0` in a single request
- Handle old session files that may have cumulative (not delta) usage values gracefully
- Clarify naming to distinguish "request" (single API call) from "turn" (user message → final response)

# Non-goals

- Adding model/pricing metadata to Usage events (low value, adds complexity)
- Schema migration tooling for old sessions
- Changing the session file format

# Design principles

- User journey drives order
- Backward compatibility over correctness for edge cases (old sessions should load, even if usage is slightly off)
- Defensive coding: assume provider behavior may change

# User journey

1. User sends a message, agent makes one or more API requests (tool-use loop)
2. Each request's usage is saved exactly once to session file
3. User interrupts or request completes → any unsaved usage is flushed
4. User reloads session → cumulative and latest usage display correctly
5. User loads an old session (pre-delta format) → session loads without crash, usage may be approximate

# Foundations / Already shipped (✅)

## Per-request delta storage
- What exists: `Usage` struct, `SessionEvent::Usage`, `extract_usage_from_events()` sums deltas
- ✅ Demo: Create session, make 2 requests, reload → cumulative = sum of both
- Gaps: No protection against duplicate saves per request

## Interrupted request handling
- What exists: `has_unsaved_usage()` + save on `TurnComplete`/`Interrupted`
- ✅ Demo: Start request, Ctrl+C before output → input tokens saved
- Gaps: None

## Session reload
- What exists: `extract_usage_from_events()` returns `(cumulative, latest)`
- ✅ Demo: `zdx sessions show <id>` displays usage, reload session in TUI shows correct context %
- Gaps: Old cumulative-style sessions would be double-counted

---

# MVP slices (ship-shaped, demoable)

## Slice 1: Prevent duplicate saves per request

- **Goal**: Ensure exactly one Usage event is saved per API request, even if provider sends multiple `UsageUpdate` events with `output_tokens > 0`
- **Scope checklist**:
  - [ ] Track whether current request has been saved (already have `request_saved` flag)
  - [ ] Only save on first `output_tokens > 0` event, not subsequent ones
  - [ ] Verify `mark_saved()` is called immediately after save in all code paths
  - [ ] Add unit test: multiple UsageUpdate events with output > 0 → only one save
- **✅ Demo**: 
  - Manually trace through code or add debug logging
  - Confirm only one `SaveSession` effect per request in tool-use loop
- **Risks / failure modes**:
  - If provider changes to send input tokens in multiple events, we might miss some (low risk, monitor)

## Slice 2: Rename "turn" to "request" in SessionUsage

- **Goal**: Clarify that `turn_*` fields track per-request values, not per-user-turn values
- **Scope checklist**:
  - [ ] Rename `turn_input` → `request_input` (and other `turn_*` fields)
  - [ ] Rename `turn_usage()` → `request_usage()`
  - [ ] Update doc comments to explain request vs turn
  - [ ] Update tests
- **✅ Demo**: 
  - Code review: all references to "turn" in SessionUsage now say "request"
  - `cargo test` passes
- **Risks / failure modes**:
  - Purely internal refactor, no runtime risk

## Slice 3: Add defensive handling for old session files

- **Goal**: Sessions created before per-request delta storage should load without panic, with best-effort usage display
- **Scope checklist**:
  - [ ] In `extract_usage_from_events()`, detect if usage values look cumulative (e.g., each event's input >= previous)
  - [ ] If detected, use last event as both cumulative and latest (heuristic)
  - [ ] Add comment explaining the heuristic and when it applies
  - [ ] Add unit test with mock old-style cumulative events
- **✅ Demo**: 
  - Create a mock session file with cumulative usage events
  - Load in TUI → no crash, usage displays (may be approximate)
- **Risks / failure modes**:
  - Heuristic may misfire on edge cases (acceptable: better than crash)
  - False positive detection could affect new sessions (mitigate with conservative threshold)

---

# Contracts (guardrails)

1. **No double-counting**: Cumulative usage after reload must equal sum of all request costs, not more
2. **No data loss**: Interrupted requests must have their input tokens saved
3. **Backward compatible load**: Any valid session file (old or new) must load without panic
4. **Context % accuracy**: After reload, context % must reflect the latest request's token usage

# Key decisions (decide early)

1. **Heuristic for old sessions**: How to detect cumulative vs delta usage?
   - Option A: Check if `input` field is monotonically increasing across events
   - Option B: Add a schema version field to Usage events (more work, cleaner)
   - **Recommendation**: Option A for MVP, consider B if edge cases emerge

2. **Multiple output_tokens > 0 events**: Is this even possible with Anthropic?
   - Need to verify provider behavior
   - If not possible, Slice 1 becomes defensive hardening only

# Testing

- **Slice 1**: Unit test simulating multiple UsageUpdate events → assert single save
- **Slice 2**: Existing tests pass after rename (no new tests needed)
- **Slice 3**: Unit test with mock cumulative-style usage events → assert no crash, reasonable values

Manual smoke tests:
- Tool-use loop (3+ requests) → reload session → verify usage
- Ctrl+C interrupt → reload → verify input tokens present
- Load very old session file (if available) → no crash

# Polish phases (after MVP)

## Phase 1: Observability
- Add debug logging for usage save events (when, what values)
- ✅ Check-in: Can trace exactly when usage is saved in logs

## Phase 2: Documentation
- Document the usage event-sourcing model in ARCHITECTURE.md
- ✅ Check-in: New contributor can understand usage flow from docs

# Later / Deferred

| Item | Trigger to revisit |
|------|-------------------|
| Model metadata in Usage events | If users report cost misprice when switching models mid-session |
| Schema version field for Usage | If old-session heuristic causes issues |
| Usage event deduplication at write time | If provider confirmed to send duplicates |
